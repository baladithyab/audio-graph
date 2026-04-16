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
    /// Tokio runtime that owns the WebSocket tasks.
    rt: Option<tokio::runtime::Runtime>,
    /// Sender for audio commands -> async writer task.
    audio_tx: Option<tokio_mpsc::UnboundedSender<AudioCmd>>,
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
            rt: None,
            audio_tx: None,
            _reader_handle: None,
            _writer_handle: None,
        }
    }

    // ------------------------------------------------------------------
    // Connect
    // ------------------------------------------------------------------

    /// Connect to the Deepgram real-time transcription API.
    ///
    /// Blocks the caller until the WebSocket is open, then spawns background
    /// reader and writer tasks on an internal tokio runtime.
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

        let url_str = format!(
            "wss://api.deepgram.com/v1/listen?\
             encoding=linear16&sample_rate=16000&channels=1\
             &model={}\
             &interim_results=true\
             &diarize={}\
             &punctuate=true",
            self.config.model,
            self.config.enable_diarization,
        );

        let api_key = self.config.api_key.clone();
        let event_tx = self.event_tx.clone();
        let connected = Arc::clone(&self.connected);

        // Perform the blocking connect inside the runtime.
        let (audio_tx, reader_handle, writer_handle) = rt.block_on(async move {
            // Build the WebSocket request with Authorization header.
            let request = tungstenite::http::Request::builder()
                .uri(&url_str)
                .header("Authorization", format!("Token {}", api_key))
                .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
                .header("Sec-WebSocket-Version", "13")
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Host", "api.deepgram.com")
                .body(())
                .map_err(|e| format!("Failed to build WebSocket request: {e}"))?;

            let (ws_stream, _response) =
                tokio_tungstenite::connect_async(request)
                    .await
                    .map_err(|e| format!("WebSocket connect failed: {e}"))?;

            let (writer, reader) = ws_stream.split();

            log::info!("Deepgram: WebSocket connected");
            connected.store(true, Ordering::SeqCst);

            // Send Connected event
            let _ = event_tx.send(DeepgramEvent::Connected);

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
        self._reader_handle = Some(reader_handle);
        self._writer_handle = Some(writer_handle);
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
    pub fn send_audio(&self, audio: &[f32]) -> Result<(), String> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err("Not connected to Deepgram".to_string());
        }

        if audio.is_empty() {
            return Ok(());
        }

        let tx = self
            .audio_tx
            .as_ref()
            .ok_or_else(|| "Audio channel not initialized".to_string())?;

        // f32 -> i16 LE PCM bytes
        let pcm_bytes = f32_to_i16_le_bytes(audio);

        tx.send(AudioCmd::Chunk(pcm_bytes))
            .map_err(|_| "Audio channel closed".to_string())
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
    /// down the internal tokio runtime.
    pub fn disconnect(&self) {
        log::info!("DeepgramStreamingClient: disconnecting");

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

/// Background task: reads from the WebSocket and emits [`DeepgramEvent`]s.
async fn reader_loop(
    mut reader: WsReader,
    tx: crossbeam_channel::Sender<DeepgramEvent>,
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
                log::info!("Deepgram: server closed connection: {frame:?}");
                let _ = tx.send(DeepgramEvent::Disconnected);
                break;
            }
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {
                // Protocol-level frames; nothing to do.
            }
            Ok(Message::Binary(_)) => {
                // Unexpected for Deepgram text-mode responses.
                log::debug!("Deepgram: unexpected binary message");
            }
            Err(tungstenite::Error::ConnectionClosed) => {
                log::info!("Deepgram: connection closed");
                let _ = tx.send(DeepgramEvent::Disconnected);
                break;
            }
            Err(e) => {
                log::error!("Deepgram: WebSocket read error: {e}");
                let _ = tx.send(DeepgramEvent::Error {
                    message: format!("WebSocket error: {e}"),
                });
                let _ = tx.send(DeepgramEvent::Disconnected);
                break;
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    log::info!("Deepgram: reader loop exited");
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
            AudioCmd::Chunk(pcm_bytes) => {
                // Deepgram expects raw binary frames of audio data.
                if let Err(e) = writer.send(Message::Binary(pcm_bytes.into())).await {
                    log::error!("Deepgram: failed to send audio: {e}");
                    break;
                }
            }
            AudioCmd::Stop => {
                // Send an empty byte message to signal end of audio.
                let _ = writer.send(Message::Binary(vec![].into())).await;
                let _ = writer.close().await;
                break;
            }
        }
    }

    connected.store(false, Ordering::SeqCst);
    log::info!("Deepgram: writer loop exited");
}

/// Parse a single Deepgram server JSON message and emit appropriate events.
fn handle_server_message(
    text: &str,
    tx: &crossbeam_channel::Sender<DeepgramEvent>,
) {
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
            let start = parsed
                .get("start")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
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
                                let word_start = w.get("start").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                let end = w.get("end").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                let conf = w.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                                let speaker = w.get("speaker").and_then(|v| v.as_u64()).map(|s| s as u32);
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

        assert!(rx.try_recv().is_err(), "Empty transcript should not emit event");
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
        ];

        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let _parsed: Value = serde_json::from_str(&json).unwrap();
        }
    }
}
