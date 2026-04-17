//! Deepgram Streaming ASR WebSocket client.
//!
//! Connects to the Deepgram real-time transcription API via WebSocket and
//! streams audio for low-latency speech-to-text with optional speaker
//! diarization.
//!
//! # Protocol overview
//!
//! 1. Open WSS connection to `wss://api.deepgram.com/v1/listen` with query
//!    parameters for encoding, sample rate, model, etc.
//! 2. Authenticate via `Authorization: Token {api_key}` header on upgrade.
//! 3. Stream binary frames of i16 LE PCM audio data.
//! 4. Receive JSON messages with transcript results (interim and final).
//! 5. Send an empty binary frame `[]` to signal end of audio, then close.
//!
//! # Threading model
//!
//! The public API is **synchronous** (called from `std::thread` workers in
//! the speech processor). Internally, a dedicated tokio runtime drives the
//! WebSocket. Audio is forwarded from the caller's thread to the async writer
//! via an unbounded `tokio::sync::mpsc` channel, and events flow back through
//! a `crossbeam_channel` that the speech processor consumes.

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::tungstenite::{self, Message};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Events emitted by the Deepgram streaming client to downstream consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DeepgramEvent {
    /// A transcript result from Deepgram.
    #[serde(rename = "transcript")]
    Transcript {
        text: String,
        confidence: f32,
        is_final: bool,
        speech_final: bool,
        start: f64,
        duration: f64,
        words: Vec<DeepgramWord>,
    },
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
    /// The client successfully re-established the WebSocket after a disconnect.
    #[serde(rename = "reconnected")]
    Reconnected,
}

/// A single word from Deepgram's response, with timing and optional speaker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepgramWord {
    pub word: String,
    pub start: f64,
    pub end: f64,
    pub confidence: f32,
    pub speaker: Option<u32>,
}

/// Configuration for a Deepgram streaming session.
#[derive(Debug, Clone)]
pub struct DeepgramConfig {
    /// Deepgram API key.
    pub api_key: String,
    /// Model name (e.g. `"nova-3"`).
    pub model: String,
    /// Whether to enable speaker diarization.
    pub enable_diarization: bool,
}

// ---------------------------------------------------------------------------
// Internal message passed from sync send_audio() -> async writer task
// ---------------------------------------------------------------------------

/// Hard cap on the audio-chunk backlog (see `pending_chunks`). At roughly one
/// chunk per 50ms from the speech processor this corresponds to ~10s of
/// audio — well beyond any healthy reconnect window, so exceeding it signals
/// either a bug or a network catastrophe. New chunks are dropped after this
/// point and `user_disconnected` is flipped so the caller sees a clean error.
const AUDIO_BUFFER_MAX_CHUNKS: usize = 200;

enum AudioCmd {
    /// Raw i16 LE PCM bytes ready to send as a binary frame.
    Chunk(Vec<u8>),
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

/// A Deepgram real-time streaming ASR client.
///
/// The public methods (`connect`, `send_audio`, `disconnect`, `event_rx`) are
/// all **synchronous** -- they block the caller's thread just long enough to
/// hand off work to the internal async runtime. This matches the threading
/// model used by the speech processor where worker threads run in `std::thread`.
pub struct DeepgramStreamingClient {
    config: DeepgramConfig,
    /// crossbeam event channel -- writer side (background reader task pushes here).
    event_tx: crossbeam_channel::Sender<DeepgramEvent>,
    /// crossbeam event channel -- reader side (speech processor consumes this).
    event_rx: crossbeam_channel::Receiver<DeepgramEvent>,
    /// Whether the WebSocket is connected.
    connected: Arc<AtomicBool>,
    /// Set to `true` when the user has explicitly called `disconnect()`.
    ///
    /// Used by the reader loop to distinguish a user-initiated teardown
    /// (do not auto-reconnect) from a network error or server close
    /// (auto-reconnect with exponential backoff).
    user_disconnected: Arc<AtomicBool>,
    /// Tokio runtime that owns the WebSocket tasks.
    rt: Option<tokio::runtime::Runtime>,
    /// Sender for audio commands -> async writer task.
    audio_tx: Option<tokio_mpsc::UnboundedSender<AudioCmd>>,
    /// Approximate count of audio chunks buffered in `audio_tx` awaiting
    /// transmission. Incremented by `send_audio`, decremented by the writer
    /// task. Used to bound memory during a prolonged reconnect cycle — we
    /// refuse to enqueue new chunks once the buffer exceeds
    /// [`AUDIO_BUFFER_MAX_CHUNKS`], which corresponds to roughly 10s of audio
    /// at the ~50ms chunk granularity the speech processor emits.
    pending_chunks: Arc<std::sync::atomic::AtomicUsize>,
    /// Handle to the reader task (for join on shutdown).
    _reader_handle: Option<tokio::task::JoinHandle<()>>,
    /// Handle to the writer task (for join on shutdown).
    _writer_handle: Option<tokio::task::JoinHandle<()>>,
}

impl DeepgramStreamingClient {
    /// Create a new (disconnected) Deepgram streaming client with the given config.
    pub fn new(config: DeepgramConfig) -> Self {
        let (event_tx, event_rx) = crossbeam_channel::bounded(256);
        Self {
            config,
            event_tx,
            event_rx,
            connected: Arc::new(AtomicBool::new(false)),
            user_disconnected: Arc::new(AtomicBool::new(false)),
            rt: None,
            audio_tx: None,
            pending_chunks: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            _reader_handle: None,
            _writer_handle: None,
        }
    }

    // ------------------------------------------------------------------
    // Connect
    // ------------------------------------------------------------------

    /// Connect to the Deepgram real-time transcription API.
    ///
    /// Blocks the caller until the WebSocket is open, then spawns a background
    /// session task on an internal tokio runtime. The session task handles
    /// audio writing, server message reading, and automatic reconnection with
    /// exponential backoff if the WebSocket drops mid-session.
    pub fn connect(&mut self) -> Result<(), String> {
        if self.config.api_key.is_empty() {
            return Err("Deepgram API key is not configured".to_string());
        }

        // Build a dedicated single-threaded tokio runtime for the WebSocket.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("deepgram-ws-rt")
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {e}"))?;

        let config = self.config.clone();
        let event_tx = self.event_tx.clone();
        let connected = Arc::clone(&self.connected);
        let user_disconnected = Arc::clone(&self.user_disconnected);
        // Reset on (re)connect so any prior teardown flag does not poison a
        // fresh session.
        user_disconnected.store(false, Ordering::SeqCst);
        // Reset any stale count from a prior session.
        self.pending_chunks
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let pending_chunks = Arc::clone(&self.pending_chunks);

        // Perform the blocking initial connect inside the runtime.
        let (audio_tx, session_handle) = rt.block_on(async move {
            // Initial connect — surfaced synchronously so the caller sees
            // auth / network errors immediately instead of through the
            // reconnect loop.
            let (writer, reader) = open_ws(&config).await?;

            log::info!("Deepgram: WebSocket connected");
            connected.store(true, Ordering::SeqCst);
            let _ = event_tx.send(DeepgramEvent::Connected);

            // Build the audio command channel the caller will push into.
            let (atx, arx) = tokio_mpsc::unbounded_channel::<AudioCmd>();

            // Spawn the session task, which owns both halves of the socket
            // and handles reconnects internally.
            let session_handle = tokio::spawn(session_task(
                writer,
                reader,
                arx,
                config,
                event_tx,
                connected,
                user_disconnected,
                Arc::clone(&pending_chunks),
            ));

            Ok::<_, String>((atx, session_handle))
        })?;

        self.audio_tx = Some(audio_tx);
        self._reader_handle = Some(session_handle);
        self._writer_handle = None;
        self.rt = Some(rt);

        Ok(())
    }

    // ------------------------------------------------------------------
    // Send audio
    // ------------------------------------------------------------------

    /// Send PCM audio data to Deepgram for processing.
    ///
    /// The audio should be **f32 mono 16 kHz** (matching the pipeline output).
    /// The method converts to 16-bit LE PCM and queues for async sending.
    /// Returns immediately (non-blocking).
    ///
    /// # Behaviour during auto-reconnect
    ///
    /// This method *does not* check the `connected` flag — only
    /// `user_disconnected`. That way, if the session task is in the middle of
    /// a reconnect cycle, audio is still queued to the unbounded channel and
    /// will be flushed to Deepgram as soon as the new socket is open. The
    /// caller never sees a spurious "Not connected" error for a transient
    /// network hiccup.
    pub fn send_audio(&self, audio: &[f32]) -> Result<(), String> {
        if self.user_disconnected.load(Ordering::SeqCst) {
            return Err("Deepgram client has been disconnected".to_string());
        }

        if audio.is_empty() {
            return Ok(());
        }

        let tx = self
            .audio_tx
            .as_ref()
            .ok_or_else(|| "Audio channel not initialized".to_string())?;

        // Drop chunks if the buffer has grown past the safety cap. This
        // protects against runaway memory usage when the WebSocket is stuck
        // in a long reconnect cycle (e.g. captive portal, network partition).
        // Flipping `user_disconnected` is deliberate: once we start losing
        // data the caller deserves to know the session is effectively dead
        // rather than silently seeing gaps in the transcript.
        let depth = self
            .pending_chunks
            .load(std::sync::atomic::Ordering::Relaxed);
        if depth >= AUDIO_BUFFER_MAX_CHUNKS {
            self.user_disconnected
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return Err(format!(
                "Deepgram audio buffer full ({depth} chunks) — likely a stuck reconnect. Restart the session."
            ));
        }

        // f32 -> i16 LE PCM bytes
        let pcm_bytes = f32_to_i16_le_bytes(audio);

        self.pending_chunks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tx.send(AudioCmd::Chunk(pcm_bytes)).map_err(|_| {
            // Restore the counter on send failure so a permanently closed
            // channel doesn't permanently skew the cap.
            self.pending_chunks
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            "Audio channel closed".to_string()
        })
    }

    // ------------------------------------------------------------------
    // Event receiver
    // ------------------------------------------------------------------

    /// Get a clone of the event receiver channel.
    ///
    /// The speech processor uses this to read `DeepgramEvent`s.
    pub fn event_rx(&self) -> crossbeam_channel::Receiver<DeepgramEvent> {
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

    /// Disconnect from Deepgram and clean up resources.
    ///
    /// Sends a close frame, waits for background tasks to finish, and shuts
    /// down the internal tokio runtime. Setting `user_disconnected` prevents
    /// the session task from attempting to auto-reconnect.
    pub fn disconnect(&self) {
        log::info!("DeepgramStreamingClient: disconnecting (user-initiated)");

        // Mark this teardown as user-initiated so the session task does not
        // try to reconnect after the close frame is observed.
        self.user_disconnected.store(true, Ordering::SeqCst);

        // Signal not connected first (stops send_audio calls).
        self.connected.store(false, Ordering::SeqCst);

        // Tell the writer task to close.
        if let Some(ref tx) = self.audio_tx {
            let _ = tx.send(AudioCmd::Stop);
        }

        // Emit Disconnected event.
        let _ = self.event_tx.send(DeepgramEvent::Disconnected);
    }
}

impl Drop for DeepgramStreamingClient {
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
            rt.shutdown_timeout(std::time::Duration::from_secs(3));
        }

        log::info!("DeepgramStreamingClient: dropped");
    }
}

// ===========================================================================
// Free functions -- async building blocks
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
    /// teardown (e.g. `GoAway`, idle timeout).
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

/// Open a fresh Deepgram WebSocket using the live [`DeepgramConfig`].
///
/// Used both for the initial connect and for each reconnect attempt. The
/// query-string-only "handshake" means a reconnect is just re-running this
/// function — no replay of a setup frame is required.
async fn open_ws(config: &DeepgramConfig) -> Result<(WsWriter, WsReader), String> {
    let url_str = format!(
        "wss://api.deepgram.com/v1/listen?\
         encoding=linear16&sample_rate=16000&channels=1\
         &model={}\
         &interim_results=true\
         &diarize={}\
         &punctuate=true",
        config.model, config.enable_diarization,
    );

    let request = tungstenite::http::Request::builder()
        .uri(&url_str)
        .header("Authorization", format!("Token {}", config.api_key))
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Host", "api.deepgram.com")
        .body(())
        .map_err(|e| format!("Failed to build WebSocket request: {e}"))?;

    let (ws_stream, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("WebSocket connect failed: {e}"))?;

    Ok(ws_stream.split())
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

/// Background task owning a single Deepgram WebSocket session, including
/// reconnect logic.
///
/// Runs the reader and writer concurrently via `tokio::select!`. When either
/// half reports a disconnect (server Close frame, tungstenite error, etc.),
/// the task:
///
/// 1. Checks the `user_disconnected` flag — if set, exits silently.
/// 2. Emits `Disconnected` + a fresh `Reconnecting { attempt }` event.
/// 3. Sleeps for the exponential backoff period (1s/2s/5s/10s).
/// 4. Calls [`open_ws`] to re-establish the socket.
/// 5. On success, emits `Reconnected` and resumes the read/write loop. The
///    audio channel (`arx`) is preserved across reconnects so the caller's
///    in-flight audio is not lost — it just buffers until the writer side
///    comes back.
/// 6. On failure, loops back to step 2 with the incremented attempt count.
/// 7. After 4 failed attempts, emits a fatal `Error` event and exits.
async fn session_task(
    initial_writer: WsWriter,
    initial_reader: WsReader,
    mut audio_rx: tokio_mpsc::UnboundedReceiver<AudioCmd>,
    config: DeepgramConfig,
    event_tx: crossbeam_channel::Sender<DeepgramEvent>,
    connected: Arc<AtomicBool>,
    user_disconnected: Arc<AtomicBool>,
    pending_chunks: Arc<std::sync::atomic::AtomicUsize>,
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
            &user_disconnected,
            &pending_chunks,
        )
        .await;

        // Any fresh disconnect resets to the "actively down" state so
        // `send_audio()` correctly starts rejecting while we recover.
        connected.store(false, Ordering::SeqCst);

        match disconnect {
            DisconnectKind::UserRequested | DisconnectKind::WriterEnded => {
                // Clean end — the user asked to stop, or we ran out of audio
                // commands because the client was dropped. Do not reconnect.
                log::info!("Deepgram session: ending ({disconnect:?})");
                let _ = event_tx.send(DeepgramEvent::Disconnected);
                break;
            }
            _ => {
                // Network-ish failure. If the user *also* asked to disconnect
                // (e.g. they hit stop just as the socket was dying), honour
                // that and skip the reconnect dance.
                if user_disconnected.load(Ordering::SeqCst) {
                    let _ = event_tx.send(DeepgramEvent::Disconnected);
                    break;
                }

                log::warn!("Deepgram session: disconnected — {disconnect:?}");
                let _ = event_tx.send(DeepgramEvent::Disconnected);

                reconnect_attempts += 1;
                let Some(backoff) = backoff_for_attempt(reconnect_attempts) else {
                    // Budget exhausted — surface a fatal error and stop.
                    log::error!(
                        "Deepgram session: reconnect budget exhausted after {} attempts",
                        reconnect_attempts - 1
                    );
                    let _ = event_tx.send(DeepgramEvent::Error {
                        message: "Deepgram reconnect attempts exhausted".into(),
                    });
                    break;
                };

                log::info!(
                    "Deepgram session: reconnecting (attempt {reconnect_attempts}, backoff {backoff}s)"
                );
                let _ = event_tx.send(DeepgramEvent::Reconnecting {
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
                                log::info!("Deepgram session: user cancelled during backoff");
                                let _ = event_tx.send(DeepgramEvent::Disconnected);
                                return;
                            }
                        }
                    }
                }

                // Attempt the reconnect. Deepgram has no setup handshake —
                // the query parameters on the URL *are* the handshake — so
                // `open_ws` is all we need.
                match open_ws(&config).await {
                    Ok((new_writer, new_reader)) => {
                        writer = new_writer;
                        reader = new_reader;
                        connected.store(true, Ordering::SeqCst);
                        log::info!("Deepgram session: reconnected on attempt {reconnect_attempts}");
                        let _ = event_tx.send(DeepgramEvent::Reconnected);
                        reconnect_attempts = 0;
                        // Loop around to resume run_io with the new halves.
                    }
                    Err(e) => {
                        log::warn!(
                            "Deepgram session: reconnect attempt {reconnect_attempts} failed: {e}"
                        );
                        let _ = event_tx.send(DeepgramEvent::Error {
                            message: format!("Reconnect attempt {reconnect_attempts} failed: {e}"),
                        });
                        // Loop back to the top; next iteration of run_io will
                        // short-circuit because the socket is broken, which
                        // naturally cycles the backoff ladder via
                        // reconnect_attempts += 1 above.
                        //
                        // To avoid that double-increment we drive the next
                        // attempt directly here: skip run_io and loop.
                        continue;
                    }
                }
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    log::info!("Deepgram: session task exited");
}

/// Pumps audio out and transcripts back for a single WebSocket instance.
///
/// Returns the classified [`DisconnectKind`] when the socket breaks or the
/// caller asks to stop. The session task above turns that into either a
/// reconnect or a clean exit.
async fn run_io(
    writer: &mut WsWriter,
    reader: &mut WsReader,
    audio_rx: &mut tokio_mpsc::UnboundedReceiver<AudioCmd>,
    event_tx: &crossbeam_channel::Sender<DeepgramEvent>,
    user_disconnected: &Arc<AtomicBool>,
    pending_chunks: &Arc<std::sync::atomic::AtomicUsize>,
) -> DisconnectKind {
    loop {
        tokio::select! {
            // Writer side: audio command from the caller.
            cmd = audio_rx.recv() => {
                match cmd {
                    Some(AudioCmd::Chunk(pcm_bytes)) => {
                        // Decrement on consumption. Keep this symmetric with
                        // the increment in `send_audio` so the backlog metric
                        // stays accurate whether the frame sends or errors out.
                        pending_chunks.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        if let Err(e) = writer.send(Message::Binary(pcm_bytes.into())).await {
                            log::error!("Deepgram: failed to send audio: {e}");
                            return DisconnectKind::NetworkError(format!("send failed: {e}"));
                        }
                    }
                    Some(AudioCmd::Stop) => {
                        // Graceful user-initiated close.
                        let _ = writer.send(Message::Binary(vec![].into())).await;
                        let _ = writer.close().await;
                        return DisconnectKind::UserRequested;
                    }
                    None => {
                        // Caller dropped the sender. No more audio will ever
                        // arrive — end the session without reconnecting.
                        let _ = writer.close().await;
                        return DisconnectKind::WriterEnded;
                    }
                }
            }

            // Reader side: inbound frame from Deepgram.
            result = reader.next() => {
                let Some(result) = result else {
                    // Reader stream ended without a Close frame — treat as a
                    // network-layer drop.
                    return DisconnectKind::NetworkError("reader stream ended".into());
                };

                match result {
                    Ok(Message::Text(text)) => {
                        handle_server_message(&text, event_tx);
                    }
                    Ok(Message::Close(frame)) => {
                        log::info!("Deepgram: server closed connection: {frame:?}");
                        // If the user was the one asking to close, honour that;
                        // otherwise classify as a server-initiated close that
                        // should trigger reconnect.
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
                        // Unexpected for Deepgram text-mode responses.
                        log::debug!("Deepgram: unexpected binary message");
                    }
                    Err(tungstenite::Error::ConnectionClosed)
                    | Err(tungstenite::Error::AlreadyClosed) => {
                        return DisconnectKind::NetworkError("connection closed".into());
                    }
                    Err(tungstenite::Error::Protocol(e)) => {
                        return DisconnectKind::ProtocolError(e.to_string());
                    }
                    Err(e) => {
                        log::error!("Deepgram: WebSocket read error: {e}");
                        return DisconnectKind::NetworkError(format!("{e}"));
                    }
                }
            }
        }
    }
}

/// Parse a single Deepgram server JSON message and emit appropriate events.
fn handle_server_message(text: &str, tx: &crossbeam_channel::Sender<DeepgramEvent>) {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("Deepgram: invalid JSON: {e}");
            let _ = tx.send(DeepgramEvent::Error {
                message: format!("Invalid server JSON: {e}"),
            });
            return;
        }
    };

    // Check message type
    let msg_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match msg_type {
        "Results" => {
            // Extract transcript data from the Deepgram response.
            let is_final = parsed
                .get("is_final")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let speech_final = parsed
                .get("speech_final")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let start = parsed.get("start").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let duration = parsed
                .get("duration")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);

            // Navigate: channel -> alternatives[0]
            let alternative = parsed
                .get("channel")
                .and_then(|ch| ch.get("alternatives"))
                .and_then(|alts| alts.as_array())
                .and_then(|alts| alts.first());

            if let Some(alt) = alternative {
                let transcript_text = alt
                    .get("transcript")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();

                let confidence = alt
                    .get("confidence")
                    .and_then(|c| c.as_f64())
                    .unwrap_or(0.0) as f32;

                // Parse words array
                let words: Vec<DeepgramWord> = alt
                    .get("words")
                    .and_then(|w| w.as_array())
                    .map(|words_arr| {
                        words_arr
                            .iter()
                            .filter_map(|w| {
                                let word = w.get("word")?.as_str()?.to_string();
                                let word_start =
                                    w.get("start").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                let end = w.get("end").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                let conf =
                                    w.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0)
                                        as f32;
                                let speaker =
                                    w.get("speaker").and_then(|v| v.as_u64()).map(|s| s as u32);
                                Some(DeepgramWord {
                                    word,
                                    start: word_start,
                                    end,
                                    confidence: conf,
                                    speaker,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                // Only emit if there's actual transcript text
                if !transcript_text.is_empty() {
                    let _ = tx.send(DeepgramEvent::Transcript {
                        text: transcript_text,
                        confidence,
                        is_final,
                        speech_final,
                        start,
                        duration,
                        words,
                    });
                }
            }
        }
        "Metadata" => {
            log::debug!("Deepgram: received metadata: {text}");
        }
        "UtteranceEnd" => {
            log::debug!("Deepgram: utterance end");
        }
        "SpeechStarted" => {
            log::debug!("Deepgram: speech started");
        }
        _ => {
            log::debug!("Deepgram: unhandled message type '{msg_type}': {text}");
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert f32 PCM samples (range -1.0 ... +1.0) to little-endian i16 bytes.
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
    fn client_new_is_disconnected() {
        let client = DeepgramStreamingClient::new(DeepgramConfig {
            api_key: "key".into(),
            model: "nova-3".into(),
            enable_diarization: true,
        });
        assert!(!client.is_connected());
    }

    #[test]
    fn connect_fails_without_api_key() {
        let mut client = DeepgramStreamingClient::new(DeepgramConfig {
            api_key: String::new(),
            model: "nova-3".into(),
            enable_diarization: false,
        });
        let result = client.connect();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("API key"));
    }

    #[test]
    fn send_audio_fails_when_disconnected() {
        let client = DeepgramStreamingClient::new(DeepgramConfig {
            api_key: "key".into(),
            model: "nova-3".into(),
            enable_diarization: false,
        });
        let result = client.send_audio(&[0.5, -0.3]);
        assert!(result.is_err());
    }

    #[test]
    fn handle_deepgram_transcript_result() {
        let (tx, rx) = crossbeam_channel::bounded(16);

        let msg = r#"{
            "type": "Results",
            "channel_index": [0, 1],
            "duration": 1.5,
            "start": 0.0,
            "is_final": true,
            "speech_final": true,
            "channel": {
                "alternatives": [{
                    "transcript": "hello world",
                    "confidence": 0.98,
                    "words": [
                        {"word": "hello", "start": 0.1, "end": 0.4, "confidence": 0.99, "speaker": 0},
                        {"word": "world", "start": 0.5, "end": 0.9, "confidence": 0.97, "speaker": 0}
                    ]
                }]
            }
        }"#;

        handle_server_message(msg, &tx);

        let event = rx.try_recv().unwrap();
        match event {
            DeepgramEvent::Transcript {
                text,
                confidence,
                is_final,
                speech_final,
                words,
                ..
            } => {
                assert_eq!(text, "hello world");
                assert!((confidence - 0.98).abs() < 0.01);
                assert!(is_final);
                assert!(speech_final);
                assert_eq!(words.len(), 2);
                assert_eq!(words[0].word, "hello");
                assert_eq!(words[0].speaker, Some(0));
                assert_eq!(words[1].word, "world");
            }
            _ => panic!("Expected Transcript event"),
        }
    }

    #[test]
    fn handle_empty_transcript_not_emitted() {
        let (tx, rx) = crossbeam_channel::bounded(16);

        let msg = r#"{
            "type": "Results",
            "channel_index": [0, 1],
            "duration": 0.5,
            "start": 0.0,
            "is_final": false,
            "speech_final": false,
            "channel": {
                "alternatives": [{
                    "transcript": "",
                    "confidence": 0.0,
                    "words": []
                }]
            }
        }"#;

        handle_server_message(msg, &tx);

        assert!(
            rx.try_recv().is_err(),
            "Empty transcript should not emit event"
        );
    }

    #[test]
    fn event_serialization_roundtrip() {
        let events = vec![
            DeepgramEvent::Transcript {
                text: "hello".into(),
                confidence: 0.95,
                is_final: true,
                speech_final: true,
                start: 0.0,
                duration: 1.0,
                words: vec![DeepgramWord {
                    word: "hello".into(),
                    start: 0.0,
                    end: 0.5,
                    confidence: 0.95,
                    speaker: Some(0),
                }],
            },
            DeepgramEvent::Error {
                message: "oops".into(),
            },
            DeepgramEvent::Connected,
            DeepgramEvent::Disconnected,
            DeepgramEvent::Reconnecting {
                attempt: 2,
                backoff_secs: 2,
            },
            DeepgramEvent::Reconnected,
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
}
