//! Application state managed by Tauri.
//!
//! `AppState` is registered with `tauri::Builder::manage()` and accessed
//! in command handlers via `State<'_, AppState>`.
//!
//! TODO(I6): Load configuration from `config/default.toml` at startup.
//! Currently all config values (channel capacities, model paths, etc.) are
//! hardcoded as defaults.  A future PR should parse the TOML file via `toml`
//! + resolve paths via `dirs` and populate `AppState` fields accordingly.
//! The `toml` and `dirs` crate dependencies have been removed until that
//! work is done to avoid carrying unused dependencies.

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use crate::audio::pipeline::ProcessedAudioChunk;
use crate::audio::{AudioCaptureManager, AudioChunk};
use crate::events::PipelineStatus;
use crate::gemini::GeminiLiveClient;
use crate::graph::entities::GraphSnapshot;
use crate::graph::extraction::RuleBasedExtractor;
use crate::graph::temporal::TemporalKnowledgeGraph;
use crate::llm::engine::ChatMessage;
use crate::llm::{ApiClient, LlmEngine, MistralRsEngine};
use crate::persistence::TranscriptWriter;

/// Transcript segment for frontend consumption.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TranscriptSegment {
    pub id: String,
    pub source_id: String,
    pub speaker_id: Option<String>,
    pub speaker_label: Option<String>,
    pub text: String,
    pub start_time: f64,
    pub end_time: f64,
    pub confidence: f32,
}

/// Audio source information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioSourceInfo {
    pub id: String,
    pub name: String,
    pub source_type: AudioSourceType,
    pub is_active: bool,
}

/// Type of audio source.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum AudioSourceType {
    SystemDefault,
    Device { device_id: String },
    Application { pid: u32, app_name: String },
}

/// Speaker information for the frontend.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpeakerInfo {
    pub id: String,
    pub label: String,
    pub color: String,
    pub total_speaking_time: f64,
    pub segment_count: u32,
}

/// Central application state, shared across Tauri commands and worker threads.
pub struct AppState {
    /// Unique session ID generated at app start (UUID v4).
    pub session_id: String,

    /// Buffer of transcript segments (most recent last).
    pub transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,

    /// Async transcript writer (appends to JSONL file on disk).
    pub transcript_writer: Arc<Mutex<Option<TranscriptWriter>>>,

    /// Current knowledge graph snapshot.
    pub graph_snapshot: Arc<RwLock<GraphSnapshot>>,

    /// Handle to the graph auto-save background thread.
    pub graph_autosave_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,

    /// Current pipeline status.
    pub pipeline_status: Arc<RwLock<PipelineStatus>>,

    /// Whether capture is currently active.
    pub is_capturing: Arc<RwLock<bool>>,

    /// Whether transcribe mode is active (AtomicBool for lock-free flag checks
    /// from the speech processor thread — fixes Bug 2: stop_transcribe now
    /// actually terminates the speech processor).
    pub is_transcribing: Arc<AtomicBool>,

    // ── Knowledge graph infrastructure ──────────────────────────────────
    /// The temporal knowledge graph engine.
    pub knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,

    /// Rule-based entity extractor (fallback when no LLM available).
    pub graph_extractor: Arc<RuleBasedExtractor>,

    /// Native LLM engine for entity extraction + chat.
    pub llm_engine: Arc<Mutex<Option<LlmEngine>>>,

    /// OpenAI-compatible API client (alternative to native LLM).
    pub api_client: Arc<Mutex<Option<ApiClient>>>,

    /// mistral.rs engine for entity extraction + chat (Candle backend).
    pub mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,

    /// Chat message history for the sidebar.
    pub chat_history: Arc<RwLock<Vec<ChatMessage>>>,

    // ── Audio capture infrastructure ────────────────────────────────────
    /// The capture manager (behind Mutex because AudioCaptureManager has &mut self methods).
    pub capture_manager: Arc<Mutex<AudioCaptureManager>>,

    /// Sender side of the raw audio channel (capture → pipeline).
    pub pipeline_tx: crossbeam_channel::Sender<AudioChunk>,

    /// Receiver side — cloneable, workers call `.clone()` to get their own handle.
    pub pipeline_rx: crossbeam_channel::Receiver<AudioChunk>,

    /// Sender for processed audio (pipeline → downstream ASR).
    pub processed_tx: crossbeam_channel::Sender<ProcessedAudioChunk>,

    /// Receiver for processed audio — used by the dispatcher thread.
    pub processed_rx: crossbeam_channel::Receiver<ProcessedAudioChunk>,

    /// Handle to the pipeline worker thread.
    pub pipeline_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,

    // ── Fan-out dispatcher (Bug 1 fix) ─────────────────────────────────
    // The pipeline emits to `processed_tx` → `processed_rx`. A dispatcher
    // thread reads from `processed_rx` and fans out to per-consumer channels
    // so both the speech processor and Gemini receive ALL chunks (not split).
    /// Per-speech-processor channel (dispatcher → speech processor).
    pub speech_audio_tx: crossbeam_channel::Sender<ProcessedAudioChunk>,
    pub speech_audio_rx: crossbeam_channel::Receiver<ProcessedAudioChunk>,

    /// Per-Gemini channel (dispatcher → Gemini audio sender).
    pub gemini_audio_tx: crossbeam_channel::Sender<ProcessedAudioChunk>,
    pub gemini_audio_rx: crossbeam_channel::Receiver<ProcessedAudioChunk>,

    /// Handle to the dispatcher thread that fans out processed audio.
    pub dispatcher_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,

    // ── Speech processing pipeline ─────────────────────────────────────
    /// Handle to the speech processor (ASR + diarization) orchestrator thread.
    pub speech_processor_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,

    /// Handle to the ASR worker thread (decoupled from accumulator).
    pub asr_worker_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,

    // ── Gemini Live pipeline ───────────────────────────────────────────────
    /// Whether the Gemini Live pipeline is active.
    pub is_gemini_active: Arc<RwLock<bool>>,

    /// The Gemini Live client instance (created on start_gemini, dropped on stop).
    pub gemini_client: Arc<Mutex<Option<GeminiLiveClient>>>,

    /// Handle to the Gemini audio sender thread.
    pub gemini_audio_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,

    /// Handle to the Gemini event receiver thread.
    pub gemini_event_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,

    // ── Settings ─────────────────────────────────────────────────────────
    /// Persisted application settings (ASR provider, LLM config, audio params).
    pub app_settings: Arc<RwLock<crate::settings::AppSettings>>,
}

impl AppState {
    /// Create a new `AppState` with empty defaults.
    pub fn new() -> Self {
        // Bounded channels prevent OOM if downstream consumers stall.
        // Capacities chosen per architecture spec:
        //   pipeline: 64 chunks (~2s of audio at 32ms/chunk)
        //   processed: 16 chunks (processing is quick)
        let (pipeline_tx, pipeline_rx) = crossbeam_channel::bounded::<AudioChunk>(64);
        let (processed_tx, processed_rx) = crossbeam_channel::bounded::<ProcessedAudioChunk>(16);

        // Per-consumer fan-out channels (Bug 1 fix):
        // Each downstream consumer gets its own channel so both receive ALL chunks.
        // Speech channel sized at 256 (~8s at 32ms/chunk) to absorb ASR latency spikes.
        let (speech_audio_tx, speech_audio_rx) =
            crossbeam_channel::bounded::<ProcessedAudioChunk>(256);
        let (gemini_audio_tx, gemini_audio_rx) =
            crossbeam_channel::bounded::<ProcessedAudioChunk>(16);

        let session_id = uuid::Uuid::new_v4().to_string();

        // Spawn transcript writer (best-effort — if base dir is unavailable, None)
        let transcript_writer = TranscriptWriter::spawn(&session_id);
        if transcript_writer.is_some() {
            log::info!("Transcript persistence enabled for session {}", session_id);
        } else {
            log::warn!("Transcript persistence disabled (could not resolve data directory)");
        }

        Self {
            session_id,
            transcript_buffer: Arc::new(RwLock::new(VecDeque::with_capacity(500))),
            transcript_writer: Arc::new(Mutex::new(transcript_writer)),
            graph_snapshot: Arc::new(RwLock::new(GraphSnapshot::default())),
            graph_autosave_thread: Arc::new(Mutex::new(None)),
            pipeline_status: Arc::new(RwLock::new(PipelineStatus::default())),
            is_capturing: Arc::new(RwLock::new(false)),
            is_transcribing: Arc::new(AtomicBool::new(false)),
            knowledge_graph: Arc::new(Mutex::new(TemporalKnowledgeGraph::new())),
            graph_extractor: Arc::new(RuleBasedExtractor::new()),
            llm_engine: Arc::new(Mutex::new(None)),
            api_client: Arc::new(Mutex::new(None)),
            mistralrs_engine: Arc::new(Mutex::new(None)),
            chat_history: Arc::new(RwLock::new(Vec::new())),
            capture_manager: Arc::new(Mutex::new(AudioCaptureManager::new())),
            pipeline_tx,
            pipeline_rx,
            processed_tx,
            processed_rx,
            speech_audio_tx,
            speech_audio_rx,
            gemini_audio_tx,
            gemini_audio_rx,
            dispatcher_thread: Arc::new(Mutex::new(None)),
            pipeline_thread: Arc::new(Mutex::new(None)),
            speech_processor_thread: Arc::new(Mutex::new(None)),
            asr_worker_thread: Arc::new(Mutex::new(None)),
            is_gemini_active: Arc::new(RwLock::new(false)),
            gemini_client: Arc::new(Mutex::new(None)),
            gemini_audio_thread: Arc::new(Mutex::new(None)),
            gemini_event_thread: Arc::new(Mutex::new(None)),
            app_settings: Arc::new(RwLock::new(crate::settings::AppSettings::default())),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
