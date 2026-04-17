//! Gemini Live API WebSocket client.
//!
//! Connects to the Gemini BidiGenerateContent streaming endpoint and exchanges
//! real-time audio (PCM → base64) for transcription + model text responses.
//!
//! # Protocol overview
//!
//! 1. Open WSS connection with API key in header (or Vertex bearer token).
//! 2. Send `BidiGenerateContentSetup` (model, generation config, system instruction).
//! 3. Wait for `setupComplete` server message.
//! 4. Stream audio as `realtimeInput.audio` (base64-encoded 16-bit LE PCM, 16 kHz mono).
//! 5. Receive `serverContent` messages containing:
//!    - `inputTranscription`  — what the user said
//!    - `modelTurn.parts[].text` — model reasoning / responses
//!    - `turnComplete` — end of a model turn
//!    - `goAway` — server requesting graceful shutdown
//! 6. Send `audioStreamEnd` to signal end of user input, then close.
//!
//! # Threading model
//!
//! The public API is **synchronous** (called from `std::thread` workers in
//! `commands.rs`). Internally, a dedicated tokio runtime drives the WebSocket.
//! Audio is forwarded from the caller's thread to the async writer via an
//! unbounded `tokio::sync::mpsc` channel, and events flow back through a
//! `crossbeam_channel` that the command layer already expects.
//!
//! # Auto-reconnect
//!
//! The session is wrapped in a `session_task` that runs the reader + writer
//! concurrently via `tokio::select!` and, on any network-layer disconnect or
//! server-initiated `goAway`/Close, automatically reconnects with exponential
//! backoff (1 s / 2 s / 5 s / 10 s, then gives up). Mirrors the pattern used
//! in [`crate::asr::deepgram`] and [`crate::asr::assemblyai`], with one extra
//! Gemini-specific step on each reconnect: [`open_ws`] re-runs the full setup
//! handshake (send `BidiGenerateContentSetup` → await `setupComplete`) before
//! returning the fresh reader/writer halves. `Reconnecting` and `Reconnected`
//! events are emitted so consumers (see `commands.rs`) can surface the state.
//!
//! Caveat: any in-flight model turn on the dead socket is **lost** — the fresh
//! socket starts from a blank `turnComplete` state, and audio queued during
//! the outage will be replayed to the new model instance as if it were a new
//! utterance. The client-side audio channel is preserved across reconnects so
//! no audio is dropped, just re-contextualised.

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, Message},
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Events emitted by the Gemini Live client to downstream consumers.
///
/// Serializable so Tauri can emit them directly to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GeminiEvent {
    /// A transcription of the user's speech (input audio).
    #[serde(rename = "transcription")]
    Transcription { text: String, is_final: bool },
    /// A model-generated response to the audio input.
    #[serde(rename = "model_response")]
    ModelResponse { text: String },
    /// The model finished its current turn.
    #[serde(rename = "turn_complete")]
    TurnComplete,
    /// A non-fatal error occurred.
    #[serde(rename = "error")]
    Error { message: String },
    /// The connection has been established.
    #[serde(rename = "connected")]
    Connected,
    /// The WebSocket connection was closed.
    #[serde(rename = "disconnected")]
    Disconnected,
    /// The client detected a disconnect and is attempting to reconnect.
    ///
    /// Emitted at the start of each reconnect attempt. `attempt` is 1-based:
    /// attempt 1 is the first retry after the initial loss.
    #[serde(rename = "reconnecting")]
    Reconnecting { attempt: u32, backoff_secs: u64 },
    /// The client successfully re-established the WebSocket (and re-ran the
    /// setup handshake) after a disconnect.
    #[serde(rename = "reconnected")]
    Reconnected,
}

/// Configuration for a Gemini Live session.
#[derive(Debug, Clone)]
pub struct GeminiConfig {
    /// Authentication mode (API key or Vertex AI with bearer token).
    pub auth: crate::settings::GeminiAuthMode,
    /// Model name (e.g. `"gemini-3.1-flash-live-preview"`).
    pub model: String,
}

// ---------------------------------------------------------------------------
// Internal message passed from sync send_audio() → async writer task
// ---------------------------------------------------------------------------

enum AudioCmd {
    /// Base64-encoded PCM chunk ready to send.
    Chunk(String),
    /// Signal end of audio stream and close.
    Stop,
}

// ---------------------------------------------------------------------------
// Type aliases for the split WebSocket halves
// ---------------------------------------------------------------------------

type WsWriter = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

type WsReader = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// A Gemini Live bidirectional streaming client.
///
/// The public methods (`connect`, `send_audio`, `disconnect`, `event_rx`) are
/// all **synchronous** — they block the caller's thread just long enough to
/// hand off work to the internal async runtime. This matches the threading
/// model used by `commands.rs` where worker threads run in `std::thread`.
pub struct GeminiLiveClient {
    config: GeminiConfig,
    /// crossbeam event channel — writer side (background reader task pushes here).
    event_tx: crossbeam_channel::Sender<GeminiEvent>,
    /// crossbeam event channel — reader side (command layer clones this).
    event_rx: crossbeam_channel::Receiver<GeminiEvent>,
    /// Whether the WebSocket is connected.
    connected: Arc<AtomicBool>,
    /// Set to `true` when the user has explicitly called `disconnect()`.
    ///
    /// Used by the session task to distinguish a user-initiated teardown
    /// (do not auto-reconnect) from a network error or server close
    /// (auto-reconnect with exponential backoff).
    user_disconnected: Arc<AtomicBool>,
    /// Tokio runtime that owns the WebSocket tasks.
    rt: Option<tokio::runtime::Runtime>,
    /// Sender for audio commands → async writer task.
    audio_tx: Option<tokio_mpsc::UnboundedSender<AudioCmd>>,
    /// Handle to the session task (owns both halves + reconnect logic).
    #[allow(dead_code)]
    session_handle: Option<tokio::task::JoinHandle<()>>,
    /// Session ID for potential session resumption. Updated from
    /// `sessionResumption` server messages; preserved across reconnects for
    /// potential future resumption wire-up.
    #[allow(dead_code)]
    session_id: Arc<std::sync::Mutex<Option<String>>>,
}

impl GeminiLiveClient {
    /// Create a new (disconnected) Gemini Live client with the given config.
    pub fn new(config: GeminiConfig) -> Self {
        let (event_tx, event_rx) = crossbeam_channel::bounded(128);
        Self {
            config,
            event_tx,
            event_rx,
            connected: Arc::new(AtomicBool::new(false)),
            user_disconnected: Arc::new(AtomicBool::new(false)),
            rt: None,
            audio_tx: None,
            session_handle: None,
            session_id: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    // ------------------------------------------------------------------
    // Connect
    // ------------------------------------------------------------------

    /// Connect to the Gemini Live API.
    ///
    /// Blocks the caller until the WebSocket is open and `setupComplete` has
    /// been received, then spawns a background session task on an internal
    /// tokio runtime. The session task handles audio writing, server message
    /// reading, and automatic reconnection with exponential backoff if the
    /// WebSocket drops mid-session (see [`session_task`]).
    pub fn connect(&mut self) -> Result<(), String> {
        // Validate auth configuration before proceeding.
        match &self.config.auth {
            crate::settings::GeminiAuthMode::ApiKey { api_key } => {
                if api_key.is_empty() {
                    return Err("Gemini API key is not configured".to_string());
                }
            }
            crate::settings::GeminiAuthMode::VertexAI {
                project_id,
                location,
                ..
            } => {
                if project_id.is_empty() || location.is_empty() {
                    return Err("Vertex AI project_id and location must be configured".to_string());
                }
            }
        }

        // Build a dedicated single-threaded tokio runtime for the WebSocket.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("gemini-ws-rt")
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {e}"))?;

        let config = self.config.clone();
        let event_tx = self.event_tx.clone();
        let connected = Arc::clone(&self.connected);
        let user_disconnected = Arc::clone(&self.user_disconnected);
        let session_id = Arc::clone(&self.session_id);
        // Reset on (re)connect so a prior teardown flag doesn't poison a
        // fresh session.
        user_disconnected.store(false, Ordering::SeqCst);

        // Perform the blocking initial connect + setup handshake inside the
        // runtime. Surfaced synchronously so the caller sees auth / network
        // errors immediately instead of through the reconnect loop.
        let (audio_tx, session_handle) = rt.block_on(async move {
            let (writer, reader, sess_id) = open_ws(&config).await?;

            if let Some(id) = sess_id {
                if let Ok(mut guard) = session_id.lock() {
                    *guard = Some(id);
                }
            }

            log::info!("Gemini Live: setup complete");
            connected.store(true, Ordering::SeqCst);

            // Send Connected event
            let _ = event_tx.send(GeminiEvent::Connected);

            // Build the audio command channel the caller will push into.
            let (atx, arx) = tokio_mpsc::unbounded_channel::<AudioCmd>();

            // Spawn the session task, which owns both halves of the socket
            // and handles reconnects (including full setup-handshake replay).
            let session_handle = tokio::spawn(session_task(
                writer,
                reader,
                arx,
                config,
                event_tx,
                connected,
                user_disconnected,
                session_id,
            ));

            Ok::<_, String>((atx, session_handle))
        })?;

        self.audio_tx = Some(audio_tx);
        self.session_handle = Some(session_handle);
        self.rt = Some(rt);

        Ok(())
    }

    // ------------------------------------------------------------------
    // Send audio
    // ------------------------------------------------------------------

    /// Send PCM audio data to Gemini for processing.
    ///
    /// The audio should be **f32 mono 16 kHz** (matching the pipeline output).
    /// The method converts to 16-bit LE PCM, base64-encodes, and queues for
    /// async sending. Returns immediately (non-blocking).
    ///
    /// # Behaviour during auto-reconnect
    ///
    /// This method *does not* check the `connected` flag — only
    /// `user_disconnected`. That way, if the session task is in the middle of
    /// a reconnect cycle, audio is still queued to the unbounded channel and
    /// will be flushed as soon as the new socket finishes its setup handshake.
    /// Callers never see a spurious "Not connected" error for a transient
    /// network hiccup. Note: the receiving model is a fresh instance, so any
    /// in-flight turn from the old socket is lost (see module-level docs).
    pub fn send_audio(&self, audio: &[f32]) -> Result<(), String> {
        if self.user_disconnected.load(Ordering::SeqCst) {
            return Err("Gemini client has been disconnected".to_string());
        }

        if audio.is_empty() {
            return Ok(());
        }

        let tx = self
            .audio_tx
            .as_ref()
            .ok_or_else(|| "Audio channel not initialized".to_string())?;

        // f32 → i16 LE PCM → base64
        let pcm_bytes = f32_to_i16_le_bytes(audio);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&pcm_bytes);

        tx.send(AudioCmd::Chunk(b64))
            .map_err(|_| "Audio channel closed".to_string())
    }

    // ------------------------------------------------------------------
    // Event receiver
    // ------------------------------------------------------------------

    /// Get a clone of the event receiver channel.
    ///
    /// The command layer uses this to read `GeminiEvent`s from a worker thread.
    pub fn event_rx(&self) -> crossbeam_channel::Receiver<GeminiEvent> {
        self.event_rx.clone()
    }

    // ------------------------------------------------------------------
    // Status
    // ------------------------------------------------------------------

    /// Check if the client is currently connected.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    // ------------------------------------------------------------------
    // Disconnect
    // ------------------------------------------------------------------

    /// Disconnect from the Gemini Live API and clean up resources.
    ///
    /// Sends `audioStreamEnd`, closes the WebSocket, waits for background
    /// tasks to finish, and shuts down the internal tokio runtime. Setting
    /// `user_disconnected` prevents the session task from attempting to
    /// auto-reconnect after the close frame is observed.
    pub fn disconnect(&self) {
        log::info!("GeminiLiveClient: disconnecting (user-initiated)");

        // Mark this teardown as user-initiated so the session task does not
        // try to reconnect after the close frame is observed.
        self.user_disconnected.store(true, Ordering::SeqCst);

        // Signal not connected first (stops send_audio calls).
        self.connected.store(false, Ordering::SeqCst);

        // Tell the writer task to send audioStreamEnd + close.
        if let Some(ref tx) = self.audio_tx {
            let _ = tx.send(AudioCmd::Stop);
        }

        // Emit Disconnected event.
        let _ = self.event_tx.send(GeminiEvent::Disconnected);

        // The runtime and task handles are cleaned up on Drop. We don't
        // block here because disconnect() is called from a Mutex guard in
        // stop_gemini and we want to avoid deadlock with the rt shutdown.
    }
}

impl Drop for GeminiLiveClient {
    fn drop(&mut self) {
        // Mark teardown as user-initiated so the session task exits cleanly
        // instead of trying to reconnect after we shut the runtime down.
        self.user_disconnected.store(true, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);

        // Signal writer to stop.
        if let Some(ref tx) = self.audio_tx {
            let _ = tx.send(AudioCmd::Stop);
        }
        self.audio_tx = None;

        // Shut down the tokio runtime (this joins background tasks).
        if let Some(rt) = self.rt.take() {
            // Give tasks a moment to finish cleanly.
            rt.shutdown_timeout(std::time::Duration::from_secs(3));
        }

        log::info!("GeminiLiveClient: dropped");
    }
}

// ===========================================================================
// Free functions — async building blocks
// ===========================================================================

/// Classifies *why* the session dropped so downstream logs / events can be
/// precise without the caller re-parsing error strings.
///
/// The inner `String` on the network variants carries the human-readable
/// reason for logging and telemetry. It is consumed through `Debug`
/// formatting on `{kind:?}`, which the dead-code lint does not track, hence
/// the allow.
#[derive(Debug)]
#[allow(dead_code)]
enum DisconnectKind {
    /// Remote server sent a Close frame. Typically a graceful server-side
    /// teardown (e.g. `goAway`, idle timeout).
    ServerClose(String),
    /// Transport-level error (TLS, TCP reset, DNS flap, tungstenite I/O).
    NetworkError(String),
    /// Protocol violation — malformed frame, invalid sequence, etc.
    ProtocolError(String),
    /// User called `disconnect()`. No reconnect attempt should be made.
    UserRequested,
    /// Writer task exhausted the audio command stream (caller dropped the
    /// sender). No reconnect — session is genuinely over.
    WriterEnded,
}

/// Build the `BidiGenerateContentSetup` JSON message.
///
/// Called once per (re)connect so reconnects see fresh `generationConfig` +
/// `system_instruction` values even if the config struct were mutated
/// between attempts.
fn build_setup_message(config: &GeminiConfig) -> Value {
    let model_path = match &config.auth {
        crate::settings::GeminiAuthMode::ApiKey { .. } => {
            format!("models/{}", config.model)
        }
        crate::settings::GeminiAuthMode::VertexAI {
            project_id,
            location,
            ..
        } => {
            format!(
                "projects/{}/locations/{}/publishers/google/models/{}",
                project_id, location, config.model,
            )
        }
    };

    json!({
        "setup": {
            "model": model_path,
            "generationConfig": {
                "responseModalities": ["TEXT"],
                "inputAudioTranscription": {}
            }
        }
    })
}

/// Open a fresh Gemini Live WebSocket using the live [`GeminiConfig`].
///
/// Unlike the Deepgram / AssemblyAI equivalents (whose handshake is entirely
/// in the upgrade request), Gemini requires a stateful setup message exchange
/// *after* the socket is open:
///
/// 1. Build URL + upgrade request based on auth mode (API key header vs.
///    Vertex bearer token).
/// 2. `connect_async` to establish the WebSocket.
/// 3. Split into reader + writer.
/// 4. Send the `BidiGenerateContentSetup` frame.
/// 5. Await `setupComplete` on the reader.
/// 6. Return `(writer, reader, session_id)`.
///
/// Used for the initial connect *and* every reconnect attempt, so the full
/// handshake is replayed on reconnect.
async fn open_ws(config: &GeminiConfig) -> Result<(WsWriter, WsReader, Option<String>), String> {
    // ── Open WebSocket ─────────────────────────────────────────────────
    let (ws_stream, _response) = match &config.auth {
        crate::settings::GeminiAuthMode::ApiKey { api_key } => {
            // Security: pass API key in header (not URL query string).
            // URLs get logged by DNS, proxies, firewalls, cert monitoring —
            // defeating TLS protection. Headers are not logged by default.
            let url_str = "wss://generativelanguage.googleapis.com/ws/\
                 google.ai.generativelanguage.v1beta.\
                 GenerativeService.BidiGenerateContent";

            let request = tungstenite::http::Request::builder()
                .uri(url_str)
                .header("x-goog-api-key", api_key)
                .header("Content-Type", "application/json")
                .body(())
                .map_err(|e| format!("Failed to build WebSocket request: {e}"))?;

            connect_async(request)
                .await
                .map_err(|e| format!("WebSocket connect failed: {e}"))?
        }
        crate::settings::GeminiAuthMode::VertexAI {
            project_id,
            location,
            service_account_path,
        } => {
            // Optionally set GOOGLE_APPLICATION_CREDENTIALS for an explicit
            // service-account key file.
            if let Some(sa_path) = service_account_path.as_deref() {
                if !sa_path.is_empty() {
                    std::env::set_var("GOOGLE_APPLICATION_CREDENTIALS", sa_path);
                }
            }

            let provider = gcp_auth::provider()
                .await
                .map_err(|e| format!("GCP auth provider init failed: {e}"))?;
            let token = provider
                .token(&["https://www.googleapis.com/auth/cloud-platform"])
                .await
                .map_err(|e| format!("Failed to obtain GCP bearer token: {e}"))?;

            let url_str = format!(
                "wss://{location}-aiplatform.googleapis.com/ws/\
                 google.cloud.aiplatform.v1beta1.\
                 LlmBidiService/BidiGenerateContent?\
                 alt=proto&key={project_id}",
            );

            let request = tungstenite::http::Request::builder()
                .uri(&url_str)
                .header("Authorization", format!("Bearer {}", token.as_str()))
                .header("Content-Type", "application/json")
                .body(())
                .map_err(|e| format!("Failed to build WebSocket request: {e}"))?;

            connect_async(request)
                .await
                .map_err(|e| format!("WebSocket connect failed: {e}"))?
        }
    };

    let (mut writer, reader) = ws_stream.split();

    // ── Send setup message ─────────────────────────────────────────────
    let setup_msg = build_setup_message(config);
    writer
        .send(Message::Text(setup_msg.to_string().into()))
        .await
        .map_err(|e| format!("Failed to send setup: {e}"))?;

    // ── Wait for setupComplete ─────────────────────────────────────────
    let (reader, session_id) = wait_for_setup_complete(reader).await?;

    Ok((writer, reader, session_id))
}

/// Wait for `setupComplete` from the server.
///
/// Returns the reader half (ownership transfer) and an optional session ID.
async fn wait_for_setup_complete(
    mut reader: WsReader,
) -> Result<(WsReader, Option<String>), String> {
    let timeout = tokio::time::Duration::from_secs(15);

    loop {
        let msg = tokio::time::timeout(timeout, reader.next())
            .await
            .map_err(|_| "Timed out waiting for setupComplete".to_string())?
            .ok_or_else(|| "WebSocket closed before setupComplete".to_string())?
            .map_err(|e| format!("WebSocket error waiting for setup: {e}"))?;

        if let Message::Text(text) = msg {
            let parsed: Value = serde_json::from_str(&text)
                .map_err(|e| format!("Invalid JSON from server: {e}"))?;

            if parsed.get("setupComplete").is_some() {
                let session_id = parsed["setupComplete"]["sessionId"]
                    .as_str()
                    .map(String::from);
                return Ok((reader, session_id));
            }

            log::debug!("Gemini Live: pre-setup message: {text}");
        }
    }
}

/// Backoff schedule per the resilience spec: 1 s, 2 s, 5 s, 10 s, then give up.
///
/// `attempt` is 1-based: 1 is the first retry after the initial disconnect.
/// Returns `None` once the budget is exhausted, which signals the session task
/// to emit a fatal error and exit.
fn backoff_for_attempt(attempt: u32) -> Option<u64> {
    match attempt {
        1 => Some(1),
        2 => Some(2),
        3 => Some(5),
        4 => Some(10),
        _ => None,
    }
}

/// Background task owning a single Gemini Live WebSocket session, including
/// reconnect logic.
///
/// Runs the reader and writer concurrently via `tokio::select!` in [`run_io`].
/// When either half reports a disconnect (server Close frame, tungstenite
/// error, etc.), the task:
///
/// 1. Checks the `user_disconnected` flag — if set, exits silently.
/// 2. Emits `Disconnected` + a fresh `Reconnecting { attempt }` event.
/// 3. Sleeps for the exponential backoff period (1s/2s/5s/10s), polling
///    `user_disconnected` every 100 ms so shutdown doesn't wait up to 10 s.
/// 4. Calls [`open_ws`] to re-establish the socket *including the full
///    setup-handshake replay* (send `BidiGenerateContentSetup` → await
///    `setupComplete`). This is the Gemini-specific bit that distinguishes
///    this reconnect path from Deepgram / AssemblyAI.
/// 5. On success, emits `Reconnected` and resumes the read/write loop. The
///    audio channel (`arx`) is preserved across reconnects so the caller's
///    in-flight audio is not lost — it just buffers until the new socket is
///    open.
/// 6. On failure, loops back to step 2 with the incremented attempt count.
/// 7. After 4 failed attempts, emits a fatal `Error` event and exits.
///
/// **Caveat**: any in-flight model turn on the dead socket is LOST. The fresh
/// socket starts from a blank `turnComplete` state and treats queued audio as
/// a brand-new utterance. Callers that care about turn boundaries should
/// handle the `Reconnecting`/`Reconnected` event pair as an implicit
/// `TurnComplete` barrier.
#[allow(clippy::too_many_arguments)]
async fn session_task(
    initial_writer: WsWriter,
    initial_reader: WsReader,
    mut audio_rx: tokio_mpsc::UnboundedReceiver<AudioCmd>,
    config: GeminiConfig,
    event_tx: crossbeam_channel::Sender<GeminiEvent>,
    connected: Arc<AtomicBool>,
    user_disconnected: Arc<AtomicBool>,
    session_id: Arc<std::sync::Mutex<Option<String>>>,
) {
    let mut writer = initial_writer;
    let mut reader = initial_reader;
    let mut reconnect_attempts: u32 = 0;

    loop {
        // Drive reader + writer concurrently until one side signals we are
        // done. `run_io` is responsible for pumping audio out and transcripts
        // back until the socket breaks or the caller sends `AudioCmd::Stop`.
        let disconnect = run_io(
            &mut writer,
            &mut reader,
            &mut audio_rx,
            &event_tx,
            &session_id,
            &user_disconnected,
        )
        .await;

        // Any fresh disconnect resets the "connected" flag so the rest of
        // the app knows we're recovering. `send_audio` tolerates this.
        connected.store(false, Ordering::SeqCst);

        match disconnect {
            DisconnectKind::UserRequested | DisconnectKind::WriterEnded => {
                // Clean end — the user asked to stop, or we ran out of audio
                // commands because the client was dropped. Do not reconnect.
                log::info!("Gemini session: ending ({disconnect:?})");
                let _ = event_tx.send(GeminiEvent::Disconnected);
                break;
            }
            _ => {
                // Network-ish failure. If the user *also* asked to disconnect
                // (e.g. they hit stop just as the socket was dying), honour
                // that and skip the reconnect dance.
                if user_disconnected.load(Ordering::SeqCst) {
                    let _ = event_tx.send(GeminiEvent::Disconnected);
                    break;
                }

                log::warn!("Gemini session: disconnected — {disconnect:?}");
                let _ = event_tx.send(GeminiEvent::Disconnected);

                reconnect_attempts += 1;
                let Some(backoff) = backoff_for_attempt(reconnect_attempts) else {
                    // Budget exhausted — surface a fatal error and stop.
                    log::error!(
                        "Gemini session: reconnect budget exhausted after {} attempts",
                        reconnect_attempts - 1
                    );
                    let _ = event_tx.send(GeminiEvent::Error {
                        message: "Gemini reconnect attempts exhausted".into(),
                    });
                    break;
                };

                log::info!(
                    "Gemini session: reconnecting (attempt {reconnect_attempts}, backoff {backoff}s)"
                );
                let _ = event_tx.send(GeminiEvent::Reconnecting {
                    attempt: reconnect_attempts,
                    backoff_secs: backoff,
                });

                // Sleep for the backoff window, but bail out early if the
                // user cancels during the wait.
                let sleep = tokio::time::sleep(Duration::from_secs(backoff));
                tokio::pin!(sleep);
                loop {
                    tokio::select! {
                        _ = &mut sleep => break,
                        _ = tokio::time::sleep(Duration::from_millis(100)) => {
                            if user_disconnected.load(Ordering::SeqCst) {
                                log::info!("Gemini session: user cancelled during backoff");
                                let _ = event_tx.send(GeminiEvent::Disconnected);
                                return;
                            }
                        }
                    }
                }

                // Attempt the reconnect. Unlike Deepgram, this also replays
                // the `BidiGenerateContentSetup` frame and waits for a fresh
                // `setupComplete` — all hidden inside `open_ws`.
                match open_ws(&config).await {
                    Ok((new_writer, new_reader, new_session_id)) => {
                        writer = new_writer;
                        reader = new_reader;
                        if let Some(id) = new_session_id {
                            if let Ok(mut guard) = session_id.lock() {
                                *guard = Some(id);
                            }
                        }
                        connected.store(true, Ordering::SeqCst);
                        log::info!("Gemini session: reconnected on attempt {reconnect_attempts}");
                        let _ = event_tx.send(GeminiEvent::Reconnected);
                        reconnect_attempts = 0;
                        // Loop around to resume run_io with the new halves.
                    }
                    Err(e) => {
                        log::warn!(
                            "Gemini session: reconnect attempt {reconnect_attempts} failed: {e}"
                        );
                        let _ = event_tx.send(GeminiEvent::Error {
                            message: format!("Reconnect attempt {reconnect_attempts} failed: {e}"),
                        });
                        // Skip run_io next iteration — just try the next
                        // backoff step directly.
                        continue;
                    }
                }
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    log::info!("Gemini: session task exited");
}

/// Pumps audio out and server events back for a single WebSocket instance.
///
/// Returns the classified [`DisconnectKind`] when the socket breaks or the
/// caller asks to stop. The [`session_task`] above turns that into either a
/// reconnect or a clean exit.
async fn run_io(
    writer: &mut WsWriter,
    reader: &mut WsReader,
    audio_rx: &mut tokio_mpsc::UnboundedReceiver<AudioCmd>,
    event_tx: &crossbeam_channel::Sender<GeminiEvent>,
    session_id: &Arc<std::sync::Mutex<Option<String>>>,
    user_disconnected: &Arc<AtomicBool>,
) -> DisconnectKind {
    loop {
        tokio::select! {
            // Writer side: audio command from the caller.
            cmd = audio_rx.recv() => {
                match cmd {
                    Some(AudioCmd::Chunk(b64)) => {
                        let payload = json!({
                            "realtimeInput": {
                                "audio": {
                                    "data": b64,
                                    "encoding": "LINEAR16",
                                    "sampleRateHertz": 16000
                                }
                            }
                        });

                        if let Err(e) = writer
                            .send(Message::Text(payload.to_string().into()))
                            .await
                        {
                            log::error!("Gemini: failed to send audio: {e}");
                            return DisconnectKind::NetworkError(format!("send failed: {e}"));
                        }
                    }
                    Some(AudioCmd::Stop) => {
                        // Graceful user-initiated close.
                        let end_msg = json!({ "realtimeInput": { "audioStreamEnd": true } });
                        let _ = writer
                            .send(Message::Text(end_msg.to_string().into()))
                            .await;
                        let _ = writer.close().await;
                        return DisconnectKind::UserRequested;
                    }
                    None => {
                        // Caller dropped the sender. No more audio will ever
                        // arrive — end without reconnecting.
                        let _ = writer.close().await;
                        return DisconnectKind::WriterEnded;
                    }
                }
            }

            // Reader side: inbound frame from Gemini.
            result = reader.next() => {
                let Some(result) = result else {
                    // Reader stream ended without a Close frame — treat as a
                    // network-layer drop.
                    return DisconnectKind::NetworkError("reader stream ended".into());
                };

                match result {
                    Ok(Message::Text(text)) => {
                        handle_server_message(&text, event_tx, session_id);
                    }
                    Ok(Message::Close(frame)) => {
                        log::info!("Gemini: server closed connection: {frame:?}");
                        if user_disconnected.load(Ordering::SeqCst) {
                            return DisconnectKind::UserRequested;
                        }
                        let reason = frame
                            .map(|f| format!("{} {}", f.code, f.reason))
                            .unwrap_or_else(|| "no frame".into());
                        return DisconnectKind::ServerClose(reason);
                    }
                    Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {
                        // Protocol-level frames; nothing to do.
                    }
                    Ok(Message::Binary(_)) => {
                        // TEXT modality only; binary is unexpected.
                        log::warn!("Gemini: unexpected binary message");
                    }
                    Err(tungstenite::Error::ConnectionClosed)
                    | Err(tungstenite::Error::AlreadyClosed) => {
                        return DisconnectKind::NetworkError("connection closed".into());
                    }
                    Err(tungstenite::Error::Protocol(e)) => {
                        return DisconnectKind::ProtocolError(e.to_string());
                    }
                    Err(e) => {
                        log::error!("Gemini: WebSocket read error: {e}");
                        return DisconnectKind::NetworkError(format!("{e}"));
                    }
                }
            }
        }
    }
}

/// Parse a single server JSON message and emit appropriate events.
fn handle_server_message(
    text: &str,
    tx: &crossbeam_channel::Sender<GeminiEvent>,
    session_id: &Arc<std::sync::Mutex<Option<String>>>,
) {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("Gemini Live: invalid JSON: {e}");
            let _ = tx.send(GeminiEvent::Error {
                message: format!("Invalid server JSON: {e}"),
            });
            return;
        }
    };

    // ── serverContent envelope ──────────────────────────────────────────
    if let Some(server_content) = parsed.get("serverContent") {
        // --- inputTranscription ────────────────────────────────────────
        if let Some(transcript) = server_content.get("inputTranscription") {
            if let Some(text_val) = transcript.get("text").and_then(|t| t.as_str()) {
                if !text_val.is_empty() {
                    let is_final = transcript
                        .get("completed")
                        .and_then(|c| c.as_bool())
                        .unwrap_or(false);
                    let _ = tx.send(GeminiEvent::Transcription {
                        text: text_val.to_string(),
                        is_final,
                    });
                }
            }
        }

        // --- modelTurn ─────────────────────────────────────────────────
        if let Some(model_turn) = server_content.get("modelTurn") {
            if let Some(parts) = model_turn.get("parts").and_then(|p| p.as_array()) {
                for part in parts {
                    if let Some(text_val) = part.get("text").and_then(|t| t.as_str()) {
                        if !text_val.is_empty() {
                            let _ = tx.send(GeminiEvent::ModelResponse {
                                text: text_val.to_string(),
                            });
                        }
                    }
                }
            }
        }

        // --- turnComplete ──────────────────────────────────────────────
        if server_content.get("turnComplete").is_some() {
            let _ = tx.send(GeminiEvent::TurnComplete);
        }

        return;
    }

    // ── goAway ─────────────────────────────────────────────────────────
    if parsed.get("goAway").is_some() {
        log::warn!("Gemini Live: received goAway — server is shutting down");
        let _ = tx.send(GeminiEvent::Error {
            message: "Server sent goAway; reconnection recommended".to_string(),
        });
        return;
    }

    // ── sessionResumption ──────────────────────────────────────────────
    if let Some(resumption) = parsed.get("sessionResumption") {
        if let Some(new_id) = resumption.get("sessionId").and_then(|s| s.as_str()) {
            if let Ok(mut guard) = session_id.lock() {
                *guard = Some(new_id.to_string());
            }
            log::info!("Gemini Live: session resumption token updated");
        }
        return;
    }

    // ── Unknown ────────────────────────────────────────────────────────
    log::debug!("Gemini Live: unhandled message: {text}");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert f32 PCM samples (range −1.0 … +1.0) to little-endian i16 bytes.
fn f32_to_i16_le_bytes(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let val = if clamped >= 0.0 {
            (clamped * i16::MAX as f32) as i16
        } else {
            (clamped * -(i16::MIN as f32)) as i16
        };
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_to_i16_conversion_silence() {
        let silence = [0.0f32; 4];
        let bytes = f32_to_i16_le_bytes(&silence);
        assert_eq!(bytes.len(), 8);
        assert!(bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn f32_to_i16_conversion_full_scale() {
        let samples = [1.0f32, -1.0];
        let bytes = f32_to_i16_le_bytes(&samples);
        assert_eq!(&bytes[0..2], &i16::MAX.to_le_bytes());
        assert_eq!(&bytes[2..4], &i16::MIN.to_le_bytes());
    }

    #[test]
    fn f32_to_i16_clamps() {
        let samples = [2.0f32, -3.0];
        let bytes = f32_to_i16_le_bytes(&samples);
        assert_eq!(&bytes[0..2], &i16::MAX.to_le_bytes());
        assert_eq!(&bytes[2..4], &i16::MIN.to_le_bytes());
    }

    #[test]
    fn setup_message_structure_api_key() {
        let config = GeminiConfig {
            auth: crate::settings::GeminiAuthMode::ApiKey {
                api_key: "test-key".into(),
            },
            model: "gemini-3.1-flash-live-preview".into(),
        };
        let msg = build_setup_message(&config);

        assert_eq!(
            msg["setup"]["model"],
            "models/gemini-3.1-flash-live-preview"
        );
        assert_eq!(
            msg["setup"]["generationConfig"]["responseModalities"][0],
            "TEXT"
        );
        assert!(msg["setup"]["generationConfig"]["inputAudioTranscription"].is_object());
    }

    #[test]
    fn setup_message_structure_vertex_ai() {
        let config = GeminiConfig {
            auth: crate::settings::GeminiAuthMode::VertexAI {
                project_id: "my-project".into(),
                location: "us-central1".into(),
                service_account_path: None,
            },
            model: "gemini-3.1-flash-live-preview".into(),
        };
        let msg = build_setup_message(&config);

        assert_eq!(
            msg["setup"]["model"],
            "projects/my-project/locations/us-central1/publishers/google/models/gemini-3.1-flash-live-preview"
        );
    }

    #[test]
    fn event_serialization_roundtrip() {
        let events = vec![
            GeminiEvent::Transcription {
                text: "hello".into(),
                is_final: true,
            },
            GeminiEvent::ModelResponse {
                text: "world".into(),
            },
            GeminiEvent::TurnComplete,
            GeminiEvent::Error {
                message: "oops".into(),
            },
            GeminiEvent::Connected,
            GeminiEvent::Disconnected,
            GeminiEvent::Reconnecting {
                attempt: 2,
                backoff_secs: 2,
            },
            GeminiEvent::Reconnected,
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let _parsed: Value = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn backoff_schedule_matches_spec() {
        // 1s, 2s, 5s, 10s, then give up.
        assert_eq!(backoff_for_attempt(1), Some(1));
        assert_eq!(backoff_for_attempt(2), Some(2));
        assert_eq!(backoff_for_attempt(3), Some(5));
        assert_eq!(backoff_for_attempt(4), Some(10));
        assert_eq!(backoff_for_attempt(5), None);
        assert_eq!(backoff_for_attempt(99), None);
    }

    #[test]
    fn client_new_is_disconnected() {
        let client = GeminiLiveClient::new(GeminiConfig {
            auth: crate::settings::GeminiAuthMode::ApiKey {
                api_key: "key".into(),
            },
            model: "model".into(),
        });
        assert!(!client.is_connected());
    }

    #[test]
    fn connect_fails_without_api_key() {
        let mut client = GeminiLiveClient::new(GeminiConfig {
            auth: crate::settings::GeminiAuthMode::ApiKey {
                api_key: String::new(),
            },
            model: "model".into(),
        });
        let result = client.connect();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("API key"));
    }

    #[test]
    fn connect_fails_without_vertex_config() {
        let mut client = GeminiLiveClient::new(GeminiConfig {
            auth: crate::settings::GeminiAuthMode::VertexAI {
                project_id: String::new(),
                location: String::new(),
                service_account_path: None,
            },
            model: "model".into(),
        });
        let result = client.connect();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("project_id"));
    }

    #[test]
    fn send_audio_fails_when_disconnected() {
        let client = GeminiLiveClient::new(GeminiConfig {
            auth: crate::settings::GeminiAuthMode::ApiKey {
                api_key: "key".into(),
            },
            model: "model".into(),
        });
        let result = client.send_audio(&[0.5, -0.3]);
        assert!(result.is_err());
    }

    #[test]
    fn handle_server_transcription() {
        let (tx, rx) = crossbeam_channel::bounded(16);
        let session_id = Arc::new(std::sync::Mutex::new(None));

        let msg = r#"{
            "serverContent": {
                "inputTranscription": {
                    "text": "hello world",
                    "completed": true
                }
            }
        }"#;

        handle_server_message(msg, &tx, &session_id);

        let event = rx.try_recv().unwrap();
        match event {
            GeminiEvent::Transcription { text, is_final } => {
                assert_eq!(text, "hello world");
                assert!(is_final);
            }
            _ => panic!("Expected Transcription event"),
        }
    }

    #[test]
    fn handle_server_model_turn() {
        let (tx, rx) = crossbeam_channel::bounded(16);
        let session_id = Arc::new(std::sync::Mutex::new(None));

        let msg = r#"{
            "serverContent": {
                "modelTurn": {
                    "parts": [
                        { "text": "The user said hello" }
                    ]
                }
            }
        }"#;

        handle_server_message(msg, &tx, &session_id);

        let event = rx.try_recv().unwrap();
        match event {
            GeminiEvent::ModelResponse { text } => {
                assert_eq!(text, "The user said hello");
            }
            _ => panic!("Expected ModelResponse event"),
        }
    }

    #[test]
    fn handle_server_turn_complete() {
        let (tx, rx) = crossbeam_channel::bounded(16);
        let session_id = Arc::new(std::sync::Mutex::new(None));

        let msg = r#"{ "serverContent": { "turnComplete": true } }"#;
        handle_server_message(msg, &tx, &session_id);

        match rx.try_recv().unwrap() {
            GeminiEvent::TurnComplete => {}
            _ => panic!("Expected TurnComplete event"),
        }
    }

    #[test]
    fn handle_server_go_away() {
        let (tx, rx) = crossbeam_channel::bounded(16);
        let session_id = Arc::new(std::sync::Mutex::new(None));

        let msg = r#"{ "goAway": {} }"#;
        handle_server_message(msg, &tx, &session_id);

        match rx.try_recv().unwrap() {
            GeminiEvent::Error { message } => {
                assert!(message.contains("goAway"));
            }
            _ => panic!("Expected Error event for goAway"),
        }
    }

    #[test]
    fn handle_session_resumption() {
        let (tx, _rx) = crossbeam_channel::bounded(16);
        let session_id = Arc::new(std::sync::Mutex::new(None));

        let msg = r#"{ "sessionResumption": { "sessionId": "abc-123" } }"#;
        handle_server_message(msg, &tx, &session_id);

        let guard = session_id.lock().unwrap();
        assert_eq!(guard.as_deref(), Some("abc-123"));
    }
}
