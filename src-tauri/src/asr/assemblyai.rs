//! AssemblyAI real-time streaming ASR client.
//!
//! Connects to the AssemblyAI real-time transcription WebSocket endpoint
//! and streams audio for live speech-to-text.
//!
//! # Protocol overview
//!
//! 1. Open WSS connection to `wss://api.assemblyai.com/v2/realtime/ws?sample_rate=16000`
//!    with `Authorization: {api_key}` header on the WebSocket upgrade request.
//! 2. Stream audio as JSON messages: `{ "audio_data": "<base64 PCM s16le>" }`.
//! 3. Receive partial transcripts: `{ "message_type": "PartialTranscript", "text": "..." }`.
//! 4. Receive final transcripts: `{ "message_type": "FinalTranscript", "text": "...", "confidence": 0.95, ... }`.
//! 5. Close session by sending `{ "terminate_session": true }`.
//!
//! # Threading model
//!
//! Same as the Gemini client: the public API is **synchronous** (called from
//! `std::thread` workers). Internally a dedicated tokio runtime drives the
//! WebSocket, with audio forwarded via `tokio::sync::mpsc` and events
//! delivered back through `crossbeam_channel`.

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

/// Events emitted by the AssemblyAI streaming client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AssemblyAIEvent {
    /// A partial (non-final) transcript of the user's speech.
    #[serde(rename = "partial_transcript")]
    PartialTranscript { text: String },
    /// A final transcript of the user's speech.
    #[serde(rename = "final_transcript")]
    FinalTranscript { text: String, confidence: f64 },
    /// The session has been terminated by the server.
    #[serde(rename = "session_terminated")]
    SessionTerminated,
    /// A non-fatal error occurred.
    #[serde(rename = "error")]
    Error { message: String },
}

/// Configuration for an AssemblyAI streaming session.
#[derive(Debug, Clone)]
pub struct AssemblyAIConfig {
    /// AssemblyAI API key.
    pub api_key: String,
    /// Whether to enable speaker diarization.
    pub enable_diarization: bool,
}

// ---------------------------------------------------------------------------
// Internal message passed from sync send_audio() -> async writer task
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

/// An AssemblyAI real-time streaming ASR client.
///
/// The public methods (`connect`, `send_audio`, `disconnect`, `event_rx`) are
/// all **synchronous** — they block the caller's thread just long enough to
/// hand off work to the internal async runtime. This matches the threading
/// model used by `commands.rs` where worker threads run in `std::thread`.
pub struct AssemblyAIClient {
    config: AssemblyAIConfig,
    /// crossbeam event channel — writer side (background reader task pushes here).
    event_tx: crossbeam_channel::Sender<AssemblyAIEvent>,
    /// crossbeam event channel — reader side (command layer clones this).
    event_rx: crossbeam_channel::Receiver<AssemblyAIEvent>,
    /// Whether the WebSocket is connected.
    connected: Arc<AtomicBool>,
    /// Tokio runtime that owns the WebSocket tasks.
    rt: Option<tokio::runtime::Runtime>,
    /// Sender for audio commands -> async writer task.
    audio_tx: Option<tokio_mpsc::UnboundedSender<AudioCmd>>,
    /// Handle to the reader task (for join on shutdown).
    #[allow(dead_code)]
    reader_handle: Option<tokio::task::JoinHandle<()>>,
    /// Handle to the writer task (for join on shutdown).
    #[allow(dead_code)]
    writer_handle: Option<tokio::task::JoinHandle<()>>,
}

impl AssemblyAIClient {
    /// Create a new (disconnected) AssemblyAI streaming client.
    pub fn new(config: AssemblyAIConfig) -> Self {
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
        }
    }

    // ------------------------------------------------------------------
    // Connect
    // ------------------------------------------------------------------

    /// Connect to the AssemblyAI real-time transcription API.
    ///
    /// Blocks the caller until the WebSocket is open, then spawns background
    /// reader and writer tasks on an internal tokio runtime.
    pub fn connect(&mut self) -> Result<(), String> {
        if self.config.api_key.is_empty() {
            return Err("AssemblyAI API key is not configured".to_string());
        }

        // Build a dedicated single-threaded tokio runtime for the WebSocket.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("assemblyai-ws-rt")
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {e}"))?;

        let api_key = self.config.api_key.clone();
        let event_tx = self.event_tx.clone();
        let connected = Arc::clone(&self.connected);

        // Perform the blocking connect inside the runtime.
        let (audio_tx, reader_handle, writer_handle) = rt.block_on(async move {
            // Build WebSocket URL
            let url_str = "wss://api.assemblyai.com/v2/realtime/ws?sample_rate=16000";

            // Build HTTP request with Authorization header for the WS upgrade.
            let request = tungstenite::http::Request::builder()
                .uri(url_str)
                .header("Authorization", &api_key)
                .body(())
                .map_err(|e| format!("Failed to build WebSocket request: {e}"))?;

            // Open WebSocket
            let (ws_stream, _response) = connect_async(request)
                .await
                .map_err(|e| format!("WebSocket connect failed: {e}"))?;

            let (writer, reader) = ws_stream.split();

            log::info!("AssemblyAI: WebSocket connected");
            connected.store(true, Ordering::SeqCst);

            // Spawn background tasks
            let (atx, arx) = tokio_mpsc::unbounded_channel::<AudioCmd>();

            let reader_handle = {
                let event_tx = event_tx.clone();
                let connected = connected.clone();
                tokio::spawn(reader_loop(reader, event_tx, connected))
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

    /// Send PCM audio data to AssemblyAI for transcription.
    ///
    /// The audio should be **f32 mono 16 kHz** (matching the pipeline output).
    /// The method converts to 16-bit LE PCM, base64-encodes, and queues for
    /// async sending. Returns immediately (non-blocking).
    pub fn send_audio(&self, audio: &[f32]) -> Result<(), String> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err("Not connected to AssemblyAI".to_string());
        }

        if audio.is_empty() {
            return Ok(());
        }

        let tx = self
            .audio_tx
            .as_ref()
            .ok_or_else(|| "Audio channel not initialized".to_string())?;

        // f32 -> i16 LE PCM -> base64
        let pcm_bytes = f32_to_i16_le_bytes(audio);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&pcm_bytes);

        tx.send(AudioCmd::Chunk(b64))
            .map_err(|_| "Audio channel closed".to_string())
    }

    // ------------------------------------------------------------------
    // Event receiver
    // ------------------------------------------------------------------

    /// Get a clone of the event receiver channel.
    pub fn event_rx(&self) -> crossbeam_channel::Receiver<AssemblyAIEvent> {
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

    /// Disconnect from the AssemblyAI API and clean up resources.
    ///
    /// Sends `terminate_session`, closes the WebSocket, and shuts down
    /// the internal tokio runtime on Drop.
    pub fn disconnect(&self) {
        log::info!("AssemblyAIClient: disconnecting");

        // Signal not connected first (stops send_audio calls).
        self.connected.store(false, Ordering::SeqCst);

        // Tell the writer task to send terminate_session + close.
        if let Some(ref tx) = self.audio_tx {
            let _ = tx.send(AudioCmd::Stop);
        }
    }
}

impl Drop for AssemblyAIClient {
    fn drop(&mut self) {
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

        log::info!("AssemblyAIClient: dropped");
    }
}

// ===========================================================================
// Free functions — async building blocks
// ===========================================================================

/// Background task: reads from the WebSocket and emits [`AssemblyAIEvent`]s.
async fn reader_loop(
    mut reader: WsReader,
    tx: crossbeam_channel::Sender<AssemblyAIEvent>,
    connected: Arc<AtomicBool>,
) {
    while let Some(result) = reader.next().await {
        if !connected.load(Ordering::Relaxed) {
            break;
        }

        match result {
            Ok(Message::Text(text)) => {
                handle_server_message(&text, &tx);
            }
            Ok(Message::Close(frame)) => {
                log::info!("AssemblyAI: server closed connection: {frame:?}");
                let _ = tx.send(AssemblyAIEvent::SessionTerminated);
                break;
            }
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {
                // Protocol-level frames; nothing to do.
            }
            Ok(Message::Binary(_)) => {
                log::warn!("AssemblyAI: unexpected binary message");
            }
            Err(tungstenite::Error::ConnectionClosed) => {
                log::info!("AssemblyAI: connection closed");
                let _ = tx.send(AssemblyAIEvent::SessionTerminated);
                break;
            }
            Err(e) => {
                log::error!("AssemblyAI: WebSocket read error: {e}");
                let _ = tx.send(AssemblyAIEvent::Error {
                    message: format!("WebSocket error: {e}"),
                });
                let _ = tx.send(AssemblyAIEvent::SessionTerminated);
                break;
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    log::info!("AssemblyAI: reader loop exited");
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
                // AssemblyAI expects audio as base64-encoded JSON, not raw binary.
                let payload = json!({
                    "audio_data": b64
                });

                if let Err(e) = writer
                    .send(Message::Text(payload.to_string().into()))
                    .await
                {
                    log::error!("AssemblyAI: failed to send audio: {e}");
                    break;
                }
            }
            AudioCmd::Stop => {
                // Send terminate_session message
                let terminate_msg = json!({ "terminate_session": true });
                let _ = writer
                    .send(Message::Text(terminate_msg.to_string().into()))
                    .await;
                let _ = writer.close().await;
                break;
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    log::info!("AssemblyAI: writer loop exited");
}

/// Parse a single server JSON message and emit appropriate events.
fn handle_server_message(
    text: &str,
    tx: &crossbeam_channel::Sender<AssemblyAIEvent>,
) {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("AssemblyAI: invalid JSON: {e}");
            let _ = tx.send(AssemblyAIEvent::Error {
                message: format!("Invalid server JSON: {e}"),
            });
            return;
        }
    };

    let message_type = parsed
        .get("message_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match message_type {
        "PartialTranscript" => {
            let text_val = parsed
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Only emit if there is actual text
            if !text_val.is_empty() {
                let _ = tx.send(AssemblyAIEvent::PartialTranscript { text: text_val });
            }
        }
        "FinalTranscript" => {
            let text_val = parsed
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let confidence = parsed
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if !text_val.is_empty() {
                let _ = tx.send(AssemblyAIEvent::FinalTranscript {
                    text: text_val,
                    confidence,
                });
            }
        }
        "SessionTerminated" => {
            let _ = tx.send(AssemblyAIEvent::SessionTerminated);
        }
        "SessionBegins" => {
            log::info!("AssemblyAI: session started");
        }
        _ => {
            // Check for error messages
            if let Some(error) = parsed.get("error").and_then(|v| v.as_str()) {
                log::error!("AssemblyAI: server error: {error}");
                let _ = tx.send(AssemblyAIEvent::Error {
                    message: error.to_string(),
                });
            } else {
                log::debug!("AssemblyAI: unhandled message type: {message_type}");
            }
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
    fn f32_to_i16_clamps() {
        let samples = [2.0f32, -3.0];
        let bytes = f32_to_i16_le_bytes(&samples);
        assert_eq!(&bytes[0..2], &i16::MAX.to_le_bytes());
        assert_eq!(&bytes[2..4], &i16::MIN.to_le_bytes());
    }

    #[test]
    fn client_new_is_disconnected() {
        let client = AssemblyAIClient::new(AssemblyAIConfig {
            api_key: "key".into(),
            enable_diarization: false,
        });
        assert!(!client.is_connected());
    }

    #[test]
    fn connect_fails_without_api_key() {
        let mut client = AssemblyAIClient::new(AssemblyAIConfig {
            api_key: String::new(),
            enable_diarization: false,
        });
        let result = client.connect();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("API key"));
    }

    #[test]
    fn send_audio_fails_when_disconnected() {
        let client = AssemblyAIClient::new(AssemblyAIConfig {
            api_key: "key".into(),
            enable_diarization: false,
        });
        let result = client.send_audio(&[0.5, -0.3]);
        assert!(result.is_err());
    }

    #[test]
    fn handle_partial_transcript() {
        let (tx, rx) = crossbeam_channel::bounded(16);

        let msg = r#"{
            "message_type": "PartialTranscript",
            "text": "hello world"
        }"#;

        handle_server_message(msg, &tx);

        let event = rx.try_recv().unwrap();
        match event {
            AssemblyAIEvent::PartialTranscript { text } => {
                assert_eq!(text, "hello world");
            }
            _ => panic!("Expected PartialTranscript event"),
        }
    }

    #[test]
    fn handle_final_transcript() {
        let (tx, rx) = crossbeam_channel::bounded(16);

        let msg = r#"{
            "message_type": "FinalTranscript",
            "text": "hello world",
            "confidence": 0.95
        }"#;

        handle_server_message(msg, &tx);

        let event = rx.try_recv().unwrap();
        match event {
            AssemblyAIEvent::FinalTranscript { text, confidence } => {
                assert_eq!(text, "hello world");
                assert!((confidence - 0.95).abs() < 0.001);
            }
            _ => panic!("Expected FinalTranscript event"),
        }
    }

    #[test]
    fn handle_session_terminated() {
        let (tx, rx) = crossbeam_channel::bounded(16);

        let msg = r#"{ "message_type": "SessionTerminated" }"#;
        handle_server_message(msg, &tx);

        match rx.try_recv().unwrap() {
            AssemblyAIEvent::SessionTerminated => {}
            _ => panic!("Expected SessionTerminated event"),
        }
    }

    #[test]
    fn handle_error_message() {
        let (tx, rx) = crossbeam_channel::bounded(16);

        let msg = r#"{ "error": "Authentication failed" }"#;
        handle_server_message(msg, &tx);

        match rx.try_recv().unwrap() {
            AssemblyAIEvent::Error { message } => {
                assert!(message.contains("Authentication failed"));
            }
            _ => panic!("Expected Error event"),
        }
    }

    #[test]
    fn empty_partial_transcript_not_emitted() {
        let (tx, rx) = crossbeam_channel::bounded(16);

        let msg = r#"{ "message_type": "PartialTranscript", "text": "" }"#;
        handle_server_message(msg, &tx);

        assert!(rx.try_recv().is_err(), "Empty partials should not be emitted");
    }

    #[test]
    fn event_serialization_roundtrip() {
        let events = vec![
            AssemblyAIEvent::PartialTranscript {
                text: "hello".into(),
            },
            AssemblyAIEvent::FinalTranscript {
                text: "hello world".into(),
                confidence: 0.95,
            },
            AssemblyAIEvent::SessionTerminated,
            AssemblyAIEvent::Error {
                message: "oops".into(),
            },
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let _parsed: Value = serde_json::from_str(&json).unwrap();
        }
    }
}
