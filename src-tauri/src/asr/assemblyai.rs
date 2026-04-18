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
use std::time::Duration;
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
    /// The client detected a disconnect and is attempting to reconnect.
    #[serde(rename = "reconnecting")]
    Reconnecting { attempt: u32, backoff_secs: u64 },
    /// The client successfully re-established the WebSocket after a disconnect.
    #[serde(rename = "reconnected")]
    Reconnected,
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

/// Hard cap on the audio-chunk backlog during a prolonged reconnect (see
/// `pending_chunks` on `AssemblyAIClient`). ~10s worth of 50ms chunks.
const AUDIO_BUFFER_MAX_CHUNKS: usize = 200;

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
    /// Set to `true` when the user has explicitly called `disconnect()`.
    /// Suppresses auto-reconnect on teardown.
    user_disconnected: Arc<AtomicBool>,
    /// Tokio runtime that owns the WebSocket tasks.
    rt: Option<tokio::runtime::Runtime>,
    /// Sender for audio commands -> async writer task.
    audio_tx: Option<tokio_mpsc::UnboundedSender<AudioCmd>>,
    /// Approximate backlog of unsent audio chunks. Bounded by
    /// `AUDIO_BUFFER_MAX_CHUNKS` — see the Deepgram client for the full
    /// reconnect-memory rationale.
    pending_chunks: Arc<std::sync::atomic::AtomicUsize>,
    /// Handle to the session task (owns both halves and reconnect logic).
    #[allow(dead_code)]
    session_handle: Option<tokio::task::JoinHandle<()>>,
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
            user_disconnected: Arc::new(AtomicBool::new(false)),
            rt: None,
            audio_tx: None,
            pending_chunks: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            session_handle: None,
        }
    }

    // ------------------------------------------------------------------
    // Connect
    // ------------------------------------------------------------------

    /// Connect to the AssemblyAI real-time transcription API.
    ///
    /// Blocks the caller until the WebSocket is open, then spawns a background
    /// session task on an internal tokio runtime. The session task handles
    /// audio writing, server message reading, and automatic reconnection with
    /// exponential backoff if the WebSocket drops mid-session.
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

        let config = self.config.clone();
        let event_tx = self.event_tx.clone();
        let connected = Arc::clone(&self.connected);
        let user_disconnected = Arc::clone(&self.user_disconnected);
        // Reset on (re)connect so a prior teardown flag does not poison a
        // fresh session.
        user_disconnected.store(false, Ordering::SeqCst);
        self.pending_chunks
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let pending_chunks = Arc::clone(&self.pending_chunks);

        // Perform the blocking initial connect inside the runtime.
        let (audio_tx, session_handle) = rt.block_on(async move {
            let (writer, reader) = open_ws(&config).await?;

            log::info!("AssemblyAI: WebSocket connected");
            connected.store(true, Ordering::SeqCst);

            let (atx, arx) = tokio_mpsc::unbounded_channel::<AudioCmd>();

            let session_handle = tokio::spawn(session_task(AssemblyAISessionCtx {
                writer,
                reader,
                audio_rx: arx,
                config,
                event_tx,
                connected,
                user_disconnected,
                pending_chunks,
            }));

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

    /// Send PCM audio data to AssemblyAI for transcription.
    ///
    /// The audio should be **f32 mono 16 kHz** (matching the pipeline output).
    /// The method converts to 16-bit LE PCM, base64-encodes, and queues for
    /// async sending. Returns immediately (non-blocking).
    ///
    /// # Behaviour during auto-reconnect
    ///
    /// Only `user_disconnected` is checked — not the transient `connected`
    /// flag — so the caller can keep streaming audio during a reconnect
    /// cycle. Queued chunks flush as soon as the new socket is open.
    pub fn send_audio(&self, audio: &[f32]) -> Result<(), String> {
        if self.user_disconnected.load(Ordering::SeqCst) {
            return Err("AssemblyAI client has been disconnected".to_string());
        }

        if audio.is_empty() {
            return Ok(());
        }

        let tx = self
            .audio_tx
            .as_ref()
            .ok_or_else(|| "Audio channel not initialized".to_string())?;

        // Bail when the backlog is past the safety cap — mirrors the Deepgram
        // client; see its comment for rationale.
        let depth = self
            .pending_chunks
            .load(std::sync::atomic::Ordering::Relaxed);
        if depth >= AUDIO_BUFFER_MAX_CHUNKS {
            self.user_disconnected
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return Err(format!(
                "AssemblyAI audio buffer full ({depth} chunks) — likely a stuck reconnect. Restart the session."
            ));
        }

        // f32 -> i16 LE PCM -> base64
        let pcm_bytes = f32_to_i16_le_bytes(audio);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&pcm_bytes);

        self.pending_chunks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tx.send(AudioCmd::Chunk(b64)).map_err(|_| {
            self.pending_chunks
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            "Audio channel closed".to_string()
        })
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
    /// the internal tokio runtime on Drop. Setting `user_disconnected`
    /// prevents the session task from attempting to auto-reconnect.
    pub fn disconnect(&self) {
        log::info!("AssemblyAIClient: disconnecting (user-initiated)");

        // Mark this teardown as user-initiated so the session task does not
        // try to reconnect after the close frame is observed.
        self.user_disconnected.store(true, Ordering::SeqCst);

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
        // Mark teardown as user-initiated so the session task exits cleanly.
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

        log::info!("AssemblyAIClient: dropped");
    }
}

// ===========================================================================
// Free functions — async building blocks
// ===========================================================================

/// Classifies *why* the session dropped so downstream logs / events can be
/// precise without the caller re-parsing error strings. See the matching
/// comment on Deepgram's `DisconnectKind` — the inner String is consumed
/// through `Debug` formatting, which the dead-code lint doesn't track.
#[derive(Debug)]
#[allow(dead_code)]
enum DisconnectKind {
    ServerClose(String),
    NetworkError(String),
    ProtocolError(String),
    UserRequested,
    WriterEnded,
}

/// Open a fresh AssemblyAI WebSocket using the live [`AssemblyAIConfig`].
///
/// Used for the initial connect and for each reconnect attempt. AssemblyAI's
/// real-time endpoint has no separate setup frame — the `Authorization`
/// header and query params on the upgrade request are the full handshake —
/// so a reconnect is just re-running this function.
async fn open_ws(config: &AssemblyAIConfig) -> Result<(WsWriter, WsReader), String> {
    let url_str = "wss://api.assemblyai.com/v2/realtime/ws?sample_rate=16000";

    let request = tungstenite::http::Request::builder()
        .uri(url_str)
        .header("Authorization", &config.api_key)
        .body(())
        .map_err(|e| format!("Failed to build WebSocket request: {e}"))?;

    let (ws_stream, _response) = connect_async(request)
        .await
        .map_err(|e| format!("WebSocket connect failed: {e}"))?;

    Ok(ws_stream.split())
}

/// Backoff schedule per the resilience spec: 1 s, 2 s, 5 s, 10 s, then give up.
fn backoff_for_attempt(attempt: u32) -> Option<u64> {
    match attempt {
        1 => Some(1),
        2 => Some(2),
        3 => Some(5),
        4 => Some(10),
        _ => None,
    }
}

/// Bundles everything `session_task` owns for a single AssemblyAI session:
/// the split WebSocket halves, the audio command receiver, live config,
/// the outbound event channel, and the three shared atomics. Collapses an
/// 8-arg function signature to one — see `speech/context.rs` for the same
/// pattern applied to the speech workers.
struct AssemblyAISessionCtx {
    writer: WsWriter,
    reader: WsReader,
    audio_rx: tokio_mpsc::UnboundedReceiver<AudioCmd>,
    config: AssemblyAIConfig,
    event_tx: crossbeam_channel::Sender<AssemblyAIEvent>,
    connected: Arc<AtomicBool>,
    user_disconnected: Arc<AtomicBool>,
    pending_chunks: Arc<std::sync::atomic::AtomicUsize>,
}

/// Background task owning a single AssemblyAI WebSocket session, including
/// reconnect logic. Mirrors the Deepgram `session_task` structure — see
/// comments there for full design rationale.
async fn session_task(ctx: AssemblyAISessionCtx) {
    let AssemblyAISessionCtx {
        writer: initial_writer,
        reader: initial_reader,
        mut audio_rx,
        config,
        event_tx,
        connected,
        user_disconnected,
        pending_chunks,
    } = ctx;

    let mut writer = initial_writer;
    let mut reader = initial_reader;
    let mut reconnect_attempts: u32 = 0;

    loop {
        let disconnect = run_io(
            &mut writer,
            &mut reader,
            &mut audio_rx,
            &event_tx,
            &user_disconnected,
            &pending_chunks,
        )
        .await;

        connected.store(false, Ordering::SeqCst);

        match disconnect {
            DisconnectKind::UserRequested | DisconnectKind::WriterEnded => {
                log::info!("AssemblyAI session: ending ({disconnect:?})");
                let _ = event_tx.send(AssemblyAIEvent::SessionTerminated);
                break;
            }
            _ => {
                if user_disconnected.load(Ordering::SeqCst) {
                    let _ = event_tx.send(AssemblyAIEvent::SessionTerminated);
                    break;
                }

                log::warn!("AssemblyAI session: disconnected — {disconnect:?}");

                reconnect_attempts += 1;
                let Some(backoff) = backoff_for_attempt(reconnect_attempts) else {
                    log::error!(
                        "AssemblyAI session: reconnect budget exhausted after {} attempts",
                        reconnect_attempts - 1
                    );
                    let _ = event_tx.send(AssemblyAIEvent::Error {
                        message: "AssemblyAI reconnect attempts exhausted".into(),
                    });
                    let _ = event_tx.send(AssemblyAIEvent::SessionTerminated);
                    break;
                };

                log::info!(
                    "AssemblyAI session: reconnecting (attempt {reconnect_attempts}, backoff {backoff}s)"
                );
                let _ = event_tx.send(AssemblyAIEvent::Reconnecting {
                    attempt: reconnect_attempts,
                    backoff_secs: backoff,
                });

                // Sleep for the backoff window but bail out early on user
                // cancellation so shutdown doesn't wait up to 10s.
                let sleep = tokio::time::sleep(Duration::from_secs(backoff));
                tokio::pin!(sleep);
                loop {
                    tokio::select! {
                        _ = &mut sleep => break,
                        _ = tokio::time::sleep(Duration::from_millis(100)) => {
                            if user_disconnected.load(Ordering::SeqCst) {
                                log::info!("AssemblyAI session: user cancelled during backoff");
                                let _ = event_tx.send(AssemblyAIEvent::SessionTerminated);
                                return;
                            }
                        }
                    }
                }

                match open_ws(&config).await {
                    Ok((new_writer, new_reader)) => {
                        writer = new_writer;
                        reader = new_reader;
                        connected.store(true, Ordering::SeqCst);
                        log::info!(
                            "AssemblyAI session: reconnected on attempt {reconnect_attempts}"
                        );
                        let _ = event_tx.send(AssemblyAIEvent::Reconnected);
                        reconnect_attempts = 0;
                    }
                    Err(e) => {
                        log::warn!(
                            "AssemblyAI session: reconnect attempt {reconnect_attempts} failed: {e}"
                        );
                        let _ = event_tx.send(AssemblyAIEvent::Error {
                            message: format!("Reconnect attempt {reconnect_attempts} failed: {e}"),
                        });
                        // Skip run_io next iteration — just try the next
                        // backoff step.
                        continue;
                    }
                }
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    log::info!("AssemblyAI: session task exited");
}

/// Pumps audio out and transcripts back for a single WebSocket instance.
///
/// Returns the classified [`DisconnectKind`] when the socket breaks or the
/// caller asks to stop. The session task turns that into either a reconnect
/// or a clean exit.
async fn run_io(
    writer: &mut WsWriter,
    reader: &mut WsReader,
    audio_rx: &mut tokio_mpsc::UnboundedReceiver<AudioCmd>,
    event_tx: &crossbeam_channel::Sender<AssemblyAIEvent>,
    user_disconnected: &Arc<AtomicBool>,
    pending_chunks: &Arc<std::sync::atomic::AtomicUsize>,
) -> DisconnectKind {
    loop {
        tokio::select! {
            cmd = audio_rx.recv() => {
                match cmd {
                    Some(AudioCmd::Chunk(b64)) => {
                        pending_chunks.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        // AssemblyAI expects audio as base64 JSON, not raw binary.
                        let payload = json!({ "audio_data": b64 });
                        if let Err(e) = writer
                            .send(Message::Text(payload.to_string().into()))
                            .await
                        {
                            log::error!("AssemblyAI: failed to send audio: {e}");
                            return DisconnectKind::NetworkError(format!("send failed: {e}"));
                        }
                    }
                    Some(AudioCmd::Stop) => {
                        // Graceful user-initiated close.
                        let terminate_msg = json!({ "terminate_session": true });
                        let _ = writer
                            .send(Message::Text(terminate_msg.to_string().into()))
                            .await;
                        let _ = writer.close().await;
                        return DisconnectKind::UserRequested;
                    }
                    None => {
                        // Caller dropped the sender — end without reconnecting.
                        let _ = writer.close().await;
                        return DisconnectKind::WriterEnded;
                    }
                }
            }

            result = reader.next() => {
                let Some(result) = result else {
                    return DisconnectKind::NetworkError("reader stream ended".into());
                };

                match result {
                    Ok(Message::Text(text)) => {
                        handle_server_message(&text, event_tx);
                    }
                    Ok(Message::Close(frame)) => {
                        log::info!("AssemblyAI: server closed connection: {frame:?}");
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
                        log::warn!("AssemblyAI: unexpected binary message");
                    }
                    Err(tungstenite::Error::ConnectionClosed)
                    | Err(tungstenite::Error::AlreadyClosed) => {
                        return DisconnectKind::NetworkError("connection closed".into());
                    }
                    Err(tungstenite::Error::Protocol(e)) => {
                        return DisconnectKind::ProtocolError(e.to_string());
                    }
                    Err(e) => {
                        log::error!("AssemblyAI: WebSocket read error: {e}");
                        return DisconnectKind::NetworkError(format!("{e}"));
                    }
                }
            }
        }
    }
}

/// Parse a single server JSON message and emit appropriate events.
fn handle_server_message(text: &str, tx: &crossbeam_channel::Sender<AssemblyAIEvent>) {
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

        assert!(
            rx.try_recv().is_err(),
            "Empty partials should not be emitted"
        );
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
            AssemblyAIEvent::Reconnecting {
                attempt: 3,
                backoff_secs: 5,
            },
            AssemblyAIEvent::Reconnected,
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let _parsed: Value = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn backoff_schedule_matches_spec() {
        assert_eq!(backoff_for_attempt(1), Some(1));
        assert_eq!(backoff_for_attempt(2), Some(2));
        assert_eq!(backoff_for_attempt(3), Some(5));
        assert_eq!(backoff_for_attempt(4), Some(10));
        assert_eq!(backoff_for_attempt(5), None);
    }
}
