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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

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

    /// Guard flag preventing concurrent `rotate_session` calls from racing.
    ///
    /// `rotate_session` uses `compare_exchange(false, true)` to claim the
    /// rotation slot; concurrent callers see `AlreadyRotating` and back off
    /// rather than double-shutting-down the transcript writer or racing on
    /// the `session_id` write lock.
    pub rotation_in_progress: Arc<AtomicBool>,
}

/// Outcome of a `rotate_session` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotateOutcome {
    /// Rotation completed; returns the previous session ID that was swapped out.
    Rotated(String),
    /// Another rotation is already in progress; returns the current session ID
    /// (which is either the target of the in-flight rotation or the pre-existing
    /// one — either way, the caller should treat it as "a rotation just happened").
    AlreadyRotating(String),
}

impl RotateOutcome {
    /// Convenience: the session ID that was swapped out if we rotated, or the
    /// current ID if rotation was skipped. Callers that just want "whatever was
    /// there before" can use this.
    pub fn previous_or_current(&self) -> &str {
        match self {
            RotateOutcome::Rotated(prev) => prev,
            RotateOutcome::AlreadyRotating(curr) => curr,
        }
    }
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
            rotation_in_progress: Arc::new(AtomicBool::new(false)),
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
    /// 1. Claims the `rotation_in_progress` guard atomically; a concurrent
    ///    rotate returns `RotateOutcome::AlreadyRotating(current_id)` without
    ///    touching state.
    /// 2. Swaps `self.session_id` under the write lock.
    /// 3. Shuts down the current transcript writer (bounded wait) and respawns
    ///    one bound to `new_session_id`. If the old writer's flush+join
    ///    exceeds the timeout, the JoinHandle is dropped and the new writer
    ///    is spawned anyway — transcript persistence is best-effort and a
    ///    slow disk must not block session rotation indefinitely.
    /// 4. The graph-autosave thread reads `session_id` via the shared
    ///    `Arc<RwLock<String>>` on each tick, so it picks up the new ID
    ///    within the next 30s without being respawned.
    ///
    /// The guard in step 1 is released on return via an RAII guard, so the
    /// flag is cleared even on early returns / panics inside step 3.
    pub fn rotate_session(&self, new_session_id: &str) -> RotateOutcome {
        // Step 1: concurrent-rotate guard. `compare_exchange(false, true)`
        // fails iff another thread already claimed it — in that case we skip
        // the rotation entirely and return the current ID. Using SeqCst to
        // pair with the Drop (which stores false) and to be maximally safe
        // about cross-thread visibility of the writer/session_id mutations.
        if self
            .rotation_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return RotateOutcome::AlreadyRotating(self.current_session_id());
        }
        // From here until the end of the function, we own the rotation slot.
        // `_guard` releases it on drop regardless of how we exit.
        let _guard = RotationGuard {
            flag: &self.rotation_in_progress,
        };

        let prev = {
            let mut guard = match self.session_id.write() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::replace(&mut *guard, new_session_id.to_string())
        };

        // Respawn transcript writer for the new session. The old writer is
        // asked to shut down gracefully; its join is bounded by
        // TRANSCRIPT_WRITER_SHUTDOWN_TIMEOUT so a stuck disk cannot block the
        // IPC caller. If the new writer fails to spawn (e.g. base dir not
        // resolvable), we leave the slot empty — transcript persistence is
        // best-effort and already handles None elsewhere.
        let mut writer_slot = match self.transcript_writer.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(old) = writer_slot.take() {
            if !old.shutdown_with_timeout(TRANSCRIPT_WRITER_SHUTDOWN_TIMEOUT) {
                log::warn!(
                    "Transcript writer for session {} did not finish flush within {:?}; \
                     dropping JoinHandle and proceeding with new writer",
                    prev,
                    TRANSCRIPT_WRITER_SHUTDOWN_TIMEOUT
                );
            }
        }
        *writer_slot = crate::persistence::TranscriptWriter::spawn(new_session_id);
        if writer_slot.is_some() {
            log::info!("Rotated transcript writer to session {}", new_session_id);
        } else {
            log::warn!(
                "Failed to spawn transcript writer for rotated session {}",
                new_session_id
            );
        }

        RotateOutcome::Rotated(prev)
    }
}

/// Bounded wait for the old transcript writer's flush+join on rotation.
///
/// Chosen empirically: 5s is long enough for a healthy BufWriter flush of any
/// realistic transcript buffer, but short enough that a wedged disk (hang, NFS
/// stall) doesn't block `new_session_cmd` from the UI. On timeout the writer
/// thread keeps running detached — it will eventually exit on its own when the
/// disk recovers; if it never does, the process is in worse shape than a
/// leaked thread handle.
const TRANSCRIPT_WRITER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// RAII guard that clears `rotation_in_progress` on drop, so early returns /
/// panics inside `rotate_session` don't wedge the flag in the set state.
struct RotationGuard<'a> {
    flag: &'a AtomicBool,
}

impl Drop for RotationGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
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
        let outcome = app.rotate_session(new_id);
        assert_eq!(
            outcome,
            RotateOutcome::Rotated(original.clone()),
            "rotate_session must report Rotated(previous_id) on first call"
        );
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
        // Only the most recent rotation needs to have landed — if the reader
        // or scheduler interleaved things such that some rotations raced
        // (hit AlreadyRotating), current_session_id() is still one of the
        // attempted values. In practice rotations are fast enough that they
        // all land sequentially; the assertion below is the strict case and
        // any flake would indicate the guard is doing its job.
        let current = app.current_session_id();
        assert!(
            current.starts_with("rotation-"),
            "final session id must be one of the rotation-N values, got {}",
            current
        );

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
    fn rotate_session_rejects_concurrent_entry() {
        // Directly exercise the compare_exchange guard by flipping the flag
        // manually. The second rotate MUST observe the flag-set state and
        // return AlreadyRotating without touching session_id or the writer.
        let app = AppState::new();
        let original = app.current_session_id();

        // Claim the slot (simulating an in-flight rotation).
        app.rotation_in_progress
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let outcome = app.rotate_session("should-not-land");
        match outcome {
            RotateOutcome::AlreadyRotating(curr) => {
                assert_eq!(
                    curr, original,
                    "AlreadyRotating must carry the unchanged current session id"
                );
            }
            RotateOutcome::Rotated(_) => {
                panic!("rotate_session must not succeed while rotation_in_progress is set");
            }
        }
        assert_eq!(
            app.current_session_id(),
            original,
            "session_id must not have changed when rotation was rejected"
        );

        // Release and confirm a subsequent rotation now succeeds.
        app.rotation_in_progress
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let outcome = app.rotate_session("now-it-lands");
        assert!(matches!(outcome, RotateOutcome::Rotated(_)));
        assert_eq!(app.current_session_id(), "now-it-lands");

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

    /// Torture test: 1000 threads alternating rotate_session / current_session_id
    /// for 10 seconds. Asserts no deadlock (10s wall-clock budget), no panic,
    /// and that the final state is readable + reflects one of the attempted IDs.
    ///
    /// Gated behind `#[ignore]` AND `RSAC_TORTURE=1` so it only runs under
    /// explicit opt-in. Without the env var, even `--ignored` makes it a
    /// no-op. Run with:
    ///
    /// ```text
    /// RSAC_TORTURE=1 cargo test --lib -- --ignored --test-threads=1 \
    ///   rotation_under_concurrent_load
    /// ```
    #[test]
    #[ignore = "torture test; gated on RSAC_TORTURE=1, run with --test-threads=1"]
    fn rotation_under_concurrent_load() {
        if std::env::var("RSAC_TORTURE").ok().as_deref() != Some("1") {
            eprintln!(
                "Skipping rotation_under_concurrent_load: set RSAC_TORTURE=1 to actually run"
            );
            return;
        }

        use std::sync::atomic::AtomicUsize;
        use std::time::{Duration, Instant};

        let app = Arc::new(AppState::new());
        let stop = Arc::new(AtomicBool::new(false));
        let rotate_ok = Arc::new(AtomicUsize::new(0));
        let rotate_skipped = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(AtomicUsize::new(0));

        let total_threads: usize = 1000;
        let mut handles = Vec::with_capacity(total_threads);

        for i in 0..total_threads {
            let app = app.clone();
            let stop = stop.clone();
            let rotate_ok = rotate_ok.clone();
            let rotate_skipped = rotate_skipped.clone();
            let reads = reads.clone();
            let h = std::thread::Builder::new()
                .name(format!("torture-{}", i))
                .spawn(move || {
                    let mut local_iter: u64 = 0;
                    while !stop.load(Ordering::SeqCst) {
                        if i % 2 == 0 {
                            // Rotate-heavy path.
                            let new_id = format!("t{}-i{}", i, local_iter);
                            match app.rotate_session(&new_id) {
                                RotateOutcome::Rotated(_) => {
                                    rotate_ok.fetch_add(1, Ordering::Relaxed);
                                }
                                RotateOutcome::AlreadyRotating(_) => {
                                    rotate_skipped.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        } else {
                            // Read-heavy path.
                            let id = app.current_session_id();
                            assert!(!id.is_empty(), "session_id must never be empty");
                            reads.fetch_add(1, Ordering::Relaxed);
                        }
                        local_iter = local_iter.wrapping_add(1);
                    }
                })
                .expect("spawn torture thread");
            handles.push(h);
        }

        let duration = Duration::from_secs(10);
        let hard_deadline = Instant::now() + duration + Duration::from_secs(5);
        std::thread::sleep(duration);
        stop.store(true, Ordering::SeqCst);

        for h in handles {
            // Per-thread deadlock guard: if we're already past the hard
            // deadline, call out the hang loudly before blocking on join.
            if Instant::now() > hard_deadline {
                panic!(
                    "torture test exceeded hard deadline of {:?}+5s — likely deadlock",
                    duration
                );
            }
            h.join().expect("torture thread panicked");
        }

        // Final state must be readable.
        let final_id = app.current_session_id();
        assert!(!final_id.is_empty(), "final session id must be non-empty");

        // Sanity: we did meaningful work (at least some rotations + reads).
        let r_ok = rotate_ok.load(Ordering::Relaxed);
        let r_skip = rotate_skipped.load(Ordering::Relaxed);
        let reads_total = reads.load(Ordering::Relaxed);
        assert!(
            r_ok > 0,
            "at least one rotation must have succeeded (got ok={}, skip={})",
            r_ok,
            r_skip
        );
        assert!(reads_total > 0, "at least one read must have happened");

        eprintln!(
            "torture summary: rotations ok={}, rotations skipped={}, reads={}",
            r_ok, r_skip, reads_total
        );

        // Drain the writer so its thread doesn't outlive the test process.
        let mut guard = app
            .transcript_writer
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(w) = guard.take() {
            // Use the bounded-timeout variant explicitly, as a smoke test of
            // the new path.
            let joined = w.shutdown_with_timeout(Duration::from_secs(3));
            assert!(joined, "writer must finish flush within 3s on drain");
        }
    }
}
