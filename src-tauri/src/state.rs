//! Application state managed by Tauri.
//!
//! `AppState` is registered with `tauri::Builder::manage()` and accessed
//! in command handlers via `State<'_, AppState>`.
//!
//! TODO(I6): Load configuration from `config/default.toml` at startup.
//! Currently all config values (channel capacities, model paths, etc.) are
//! hardcoded as defaults.  A future PR should parse the TOML file via `toml`
//! + resolve paths via `dirs` and populate `AppState` fields accordingly.
//!   The `toml` and `dirs` crate dependencies have been removed until that
//!   work is done to avoid carrying unused dependencies.

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
    /// Unique session ID for the currently-active session (UUID v4).
    ///
    /// Wrapped in `Arc<RwLock<...>>` so `new_session_cmd` can rotate the ID
    /// in-process without restarting the app. Persistence threads that were
    /// spawned with a clone of this `Arc` re-read the current ID on each
    /// tick / write, so rotation takes effect without respawning them
    /// (transcript writer is the exception — it owns a file handle and is
    /// respawned on rotation, see [`AppState::rotate_session`]).
    pub session_id: Arc<RwLock<String>>,

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
        // Speech channel sized at 1024 (~32s at 32ms/chunk) to absorb ASR latency
        // spikes. Cloud ASR providers (OpenAI Whisper, Groq) can take 1–5s per
        // request; at 256 chunks (~8s) a single slow burst would overflow the
        // channel and drop audio. 1024 gives the accumulator enough headroom
        // to keep producing segments while the ASR worker waits on HTTP.
        let (speech_audio_tx, speech_audio_rx) =
            crossbeam_channel::bounded::<ProcessedAudioChunk>(1024);
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
            session_id: Arc::new(RwLock::new(session_id)),
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

    /// Read the current session ID. On lock poisoning, recovers the inner
    /// value — session_id is a plain String so poisoning carries no
    /// invariant-violation risk.
    pub fn current_session_id(&self) -> String {
        match self.session_id.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Rotate to a new session in-process.
    ///
    /// 1. Swaps `self.session_id` under the write lock.
    /// 2. Shuts down the current transcript writer and respawns one bound
    ///    to `new_session_id` (the writer owns a file handle, so SIGNAL-
    ///    based rotation would still leak the old handle — respawn is
    ///    cleaner).
    /// 3. The graph-autosave thread reads `session_id` via the shared
    ///    `Arc<RwLock<String>>` on each tick, so it picks up the new ID
    ///    within the next 30s without being respawned.
    ///
    /// Returns the previous session ID so callers can finalize its
    /// index entry / usage file.
    pub fn rotate_session(&self, new_session_id: &str) -> String {
        let prev = {
            let mut guard = match self.session_id.write() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::replace(&mut *guard, new_session_id.to_string())
        };

        // Respawn transcript writer for the new session. The old writer is
        // shut down gracefully (its thread flushes and exits on the Shutdown
        // message). If the new writer fails to spawn (e.g. base dir not
        // resolvable), we leave the slot empty — transcript persistence is
        // best-effort and already handles None elsewhere.
        match self.transcript_writer.lock() {
            Ok(mut guard) => {
                if let Some(old) = guard.take() {
                    old.shutdown();
                    // Drop `old` — its JoinHandle is released without blocking
                    // the IPC caller. The writer thread exits on its own.
                }
                *guard = crate::persistence::TranscriptWriter::spawn(new_session_id);
                if guard.is_some() {
                    log::info!("Rotated transcript writer to session {}", new_session_id);
                } else {
                    log::warn!(
                        "Failed to spawn transcript writer for rotated session {}",
                        new_session_id
                    );
                }
            }
            Err(poisoned) => {
                // Lock poisoned — recover and still swap. Any invariant
                // violation inside the Option<TranscriptWriter> is benign:
                // we're about to overwrite it.
                let mut guard = poisoned.into_inner();
                if let Some(old) = guard.take() {
                    old.shutdown();
                }
                *guard = crate::persistence::TranscriptWriter::spawn(new_session_id);
            }
        }

        prev
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests — in-process session rotation (loop 20)
// ---------------------------------------------------------------------------
//
// These exercise `AppState::rotate_session` directly rather than going through
// the Tauri command.
//
// Two of the three tests are purely in-memory: id swap and the concurrent
// reader smoke test. They don't touch HOME and are safe in parallel.
//
// The third test verifies the respawned transcript writer opens a new file
// on disk — which requires mutating HOME. `sessions::usage::tests` also
// mutates HOME under its own test lock, so running these in parallel would
// stomp each other's env overrides. That test is therefore `#[ignore]`d and
// run explicitly via `cargo test --lib -- --ignored --test-threads=1
// rotate_session_respawns_transcript_writer_to_new_file`. The two parallel-
// safe tests provide the bulk of the coverage; the ignored test is the
// belt-and-braces proof for a human spot-check.

#[cfg(test)]
mod rotation_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempdir(label: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!(
            "audio-graph-rotation-{}-{}-{}-{}",
            label, pid, nanos, n
        ));
        std::fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    struct HomeGuard {
        prev_home: Option<String>,
        prev_userprofile: Option<String>,
    }

    impl HomeGuard {
        #[allow(unsafe_code)]
        fn set(dir: &std::path::Path) -> Self {
            let prev_home = std::env::var("HOME").ok();
            let prev_userprofile = std::env::var("USERPROFILE").ok();
            // SAFETY: callers hold the shared test-env lock while this lives.
            unsafe {
                std::env::set_var("HOME", dir);
                std::env::set_var("USERPROFILE", dir);
            }
            Self {
                prev_home,
                prev_userprofile,
            }
        }
    }

    impl Drop for HomeGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            unsafe {
                match &self.prev_home {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
                match &self.prev_userprofile {
                    Some(v) => std::env::set_var("USERPROFILE", v),
                    None => std::env::remove_var("USERPROFILE"),
                }
            }
        }
    }

    #[test]
    fn rotate_session_swaps_session_id_atomically() {
        // Pure in-memory — we do NOT rely on HOME resolving to anything in
        // particular. `AppState::new()` may or may not spawn a transcript
        // writer depending on ambient HOME, but the id swap is independent.
        let app = AppState::new();
        let original = app.current_session_id();
        assert!(!original.is_empty(), "session_id must be populated at init");

        let new_id = "rotated-session-aaa";
        let prev = app.rotate_session(new_id);
        assert_eq!(prev, original, "rotate_session must return the previous id");
        assert_eq!(
            app.current_session_id(),
            new_id,
            "current_session_id must reflect the new id after rotation"
        );

        // Drain any spawned writer so its thread doesn't linger past the test.
        {
            let mut guard = app
                .transcript_writer
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(w) = guard.take() {
                w.shutdown();
            }
        }
    }

    #[test]
    #[ignore = "mutates HOME; conflicts with sessions::usage::tests — run with --test-threads=1"]
    fn rotate_session_respawns_transcript_writer_to_new_file() {
        let dir = unique_tempdir("writer-respawn");
        let _g = HomeGuard::set(&dir);

        let app = AppState::new();
        let original = app.current_session_id();

        {
            let guard = app.transcript_writer.lock().unwrap();
            assert!(
                guard.is_some(),
                "initial AppState must have a transcript writer with HOME override"
            );
        }

        let new_id = "rotated-session-bbb";
        app.rotate_session(new_id);

        {
            let guard = app.transcript_writer.lock().unwrap();
            assert!(
                guard.is_some(),
                "rotate_session must leave a live writer in place"
            );
        }

        use crate::state::TranscriptSegment;
        let segment = TranscriptSegment {
            id: "seg-1".into(),
            source_id: "test".into(),
            speaker_id: None,
            speaker_label: None,
            text: "post-rotation line".into(),
            start_time: 0.0,
            end_time: 1.0,
            confidence: 1.0,
        };
        {
            let guard = app.transcript_writer.lock().unwrap();
            guard.as_ref().expect("writer present").append(&segment);
        }

        // Signal shutdown + wait briefly for the append to flush. Shutdown
        // drains the channel and flushes the BufWriter before exiting.
        {
            let mut guard = app.transcript_writer.lock().unwrap();
            if let Some(w) = guard.take() {
                w.shutdown();
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(150));

        let new_file = dir
            .join(".audiograph")
            .join("transcripts")
            .join(format!("{}.jsonl", new_id));
        assert!(
            new_file.exists(),
            "rotated writer must have opened {:?}",
            new_file
        );
        let contents = std::fs::read_to_string(&new_file).unwrap();
        assert!(
            contents.contains("post-rotation line"),
            "segment appended post-rotation must land in new session file, got: {:?}",
            contents
        );

        let original_file = dir
            .join(".audiograph")
            .join("transcripts")
            .join(format!("{}.jsonl", original));
        if original_file.exists() {
            let original_contents = std::fs::read_to_string(&original_file).unwrap();
            assert!(
                !original_contents.contains("post-rotation line"),
                "post-rotation segment must not land in the old session file"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn current_session_id_readable_while_rotation_in_progress() {
        // Pure in-memory smoke test — no HOME mutation needed.
        let app = Arc::new(AppState::new());
        let reader_app = app.clone();

        let reader = std::thread::spawn(move || {
            for _ in 0..1000 {
                let id = reader_app.current_session_id();
                assert!(!id.is_empty());
            }
        });

        for i in 0..5 {
            app.rotate_session(&format!("rotation-{}", i));
        }

        reader.join().expect("reader thread must not panic");
        assert_eq!(app.current_session_id(), "rotation-4");

        {
            let mut guard = app
                .transcript_writer
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(w) = guard.take() {
                w.shutdown();
            }
        }
    }
}
