//! Gemini Live API WebSocket client.
//!
//! Connects to the Gemini BidiGenerateContent streaming endpoint and exchanges
//! real-time audio (PCM → base64) for transcription + model text responses.
//!
//! # Protocol overview
//!
//! 1. Open WSS connection with API key in query string.
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

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
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
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    Message,
>;

type WsReader = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
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
    /// Tokio runtime that owns the WebSocket tasks.
    rt: Option<tokio::runtime::Runtime>,
    /// Sender for audio commands → async writer task.
    audio_tx: Option<tokio_mpsc::UnboundedSender<AudioCmd>>,
    /// Handle to the reader task (for join on shutdown).
    reader_handle: Option<tokio::task::JoinHandle<()>>,
    /// Handle to the writer task (for join on shutdown).
    writer_handle: Option<tokio::task::JoinHandle<()>>,
    /// Session ID for potential session resumption.
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
            rt: None,
            audio_tx: None,
            reader_handle: None,
            writer_handle: None,
            session_id: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    // ------------------------------------------------------------------
    // Connect
    // ------------------------------------------------------------------

    /// Connect to the Gemini Live API.
    ///
    /// Blocks the caller until the WebSocket is open and `setupComplete` has
    /// been received, then spawns background reader and writer tasks on an
    /// internal tokio runtime.
    pub fn connect(&mut self) -> Result<(), String> {
        // Validate auth configuration before proceeding.
        match &self.config.auth {
            crate::settings::GeminiAuthMode::ApiKey { api_key } => {
                if api_key.is_empty() {
                    return Err("Gemini API key is not configured".to_string());
                }
            }
            crate::settings::GeminiAuthMode::VertexAI { project_id, location, .. } => {
                if project_id.is_empty() || location.is_empty() {
                    return Err(
                        "Vertex AI project_id and location must be configured".to_string(),
                    );
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

        let auth = self.config.auth.clone();
        let setup_msg = build_setup_message(&self.config);
        let event_tx = self.event_tx.clone();
        let connected = Arc::clone(&self.connected);
        let session_id = Arc::clone(&self.session_id);

        // Perform the blocking connect + setup handshake inside the runtime.
        let (audio_tx, reader_handle, writer_handle) = rt.block_on(async move {
            // ── Open WebSocket ─────────────────────────────────────────
            let (ws_stream, _response) = match &auth {
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
                    // Optionally set GOOGLE_APPLICATION_CREDENTIALS for
                    // explicit service-account key file.
                    if let Some(sa_path) = service_account_path.as_deref() {
                        if !sa_path.is_empty() {
                            std::env::set_var(
                                "GOOGLE_APPLICATION_CREDENTIALS",
                                sa_path,
                            );
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
                        .header(
                            "Authorization",
                            format!("Bearer {}", token.as_str()),
                        )
                        .header("Content-Type", "application/json")
                        .body(())
                        .map_err(|e| format!("Failed to build WebSocket request: {e}"))?;

                    connect_async(request)
                        .await
                        .map_err(|e| format!("WebSocket connect failed: {e}"))?
                }
            };

            let (mut writer, reader) = ws_stream.split();

            // ── Send setup message ─────────────────────────────────────
            writer
                .send(Message::Text(setup_msg.to_string().into()))
                .await
                .map_err(|e| format!("Failed to send setup: {e}"))?;

            // ── Wait for setupComplete ─────────────────────────────────
            let (reader, sess_id) = wait_for_setup_complete(reader).await?;

            if let Some(id) = sess_id {
                if let Ok(mut guard) = session_id.lock() {
                    *guard = Some(id);
                }
            }

            log::info!("Gemini Live: setup complete");
            connected.store(true, Ordering::SeqCst);

            // Send Connected event
            let _ = event_tx.send(GeminiEvent::Connected);

            // ── Spawn background tasks ─────────────────────────────────
            let (atx, arx) = tokio_mpsc::unbounded_channel::<AudioCmd>();

            let reader_handle = {
                let event_tx = event_tx.clone();
                let connected = connected.clone();
                let session_id = session_id.clone();
                tokio::spawn(reader_loop(reader, event_tx, connected, session_id))
            };

            let writer_handle = {
                let connected = connected.clone();
                tokio::spawn(writer_loop(writer, arx, connected))
            };

            Ok::<_, String>((atx, reader_handle, writer_handle))
        })?;

        self.audio_tx = Some(audio_tx);
        self.reader_handle = Some(reader_handle);
        self.writer_handle = Some(writer_handle);
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
    pub fn send_audio(&self, audio: &[f32]) -> Result<(), String> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err("Not connected to Gemini".to_string());
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
    /// tasks to finish, and shuts down the internal tokio runtime.
    pub fn disconnect(&self) {
        log::info!("GeminiLiveClient: disconnecting");

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

/// Build the `BidiGenerateContentSetup` JSON message.
fn build_setup_message(config: &GeminiConfig) -> Value {
    let model_path = match &config.auth {
        crate::settings::GeminiAuthMode::ApiKey { .. } => {
            format!("models/{}", config.model)
        }
        crate::settings::GeminiAuthMode::VertexAI { project_id, location, .. } => {
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

/// Background task: reads from the WebSocket and emits [`GeminiEvent`]s.
async fn reader_loop(
    mut reader: WsReader,
    tx: crossbeam_channel::Sender<GeminiEvent>,
    connected: Arc<AtomicBool>,
    session_id: Arc<std::sync::Mutex<Option<String>>>,
) {
    while let Some(result) = reader.next().await {
        if !connected.load(Ordering::Relaxed) {
            break;
        }

        match result {
            Ok(Message::Text(text)) => {
                handle_server_message(&text, &tx, &session_id);
            }
            Ok(Message::Close(frame)) => {
                log::info!("Gemini Live: server closed connection: {frame:?}");
                let _ = tx.send(GeminiEvent::Disconnected);
                break;
            }
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {
                // Protocol-level frames; nothing to do.
            }
            Ok(Message::Binary(_)) => {
                // TEXT modality only; binary is unexpected.
                log::warn!("Gemini Live: unexpected binary message");
            }
            Err(tungstenite::Error::ConnectionClosed) => {
                log::info!("Gemini Live: connection closed");
                let _ = tx.send(GeminiEvent::Disconnected);
                break;
            }
            Err(e) => {
                log::error!("Gemini Live: WebSocket read error: {e}");
                let _ = tx.send(GeminiEvent::Error {
                    message: format!("WebSocket error: {e}"),
                });
                let _ = tx.send(GeminiEvent::Disconnected);
                break;
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    log::info!("Gemini Live: reader loop exited");
}

/// Background task: reads audio commands from the channel and writes to WebSocket.
async fn writer_loop(
    mut writer: WsWriter,
    mut rx: tokio_mpsc::UnboundedReceiver<AudioCmd>,
    connected: Arc<AtomicBool>,
) {
    while let Some(cmd) = rx.recv().await {
        if !connected.load(Ordering::Relaxed) {
            break;
        }

        match cmd {
            AudioCmd::Chunk(b64) => {
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
                    log::error!("Gemini Live: failed to send audio: {e}");
                    break;
                }
            }
            AudioCmd::Stop => {
                // Send audioStreamEnd
                let end_msg = json!({ "realtimeInput": { "audioStreamEnd": true } });
                let _ = writer
                    .send(Message::Text(end_msg.to_string().into()))
                    .await;
                let _ = writer.close().await;
                break;
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    log::info!("Gemini Live: writer loop exited");
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
        assert!(
            msg["setup"]["generationConfig"]["inputAudioTranscription"].is_object()
        );
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
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let _parsed: Value = serde_json::from_str(&json).unwrap();
        }
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
