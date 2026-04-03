//! Speech processing orchestrator.
//!
//! Contains the speech processor logic (ASR + diarization + entity extraction)
//! extracted from `commands.rs` to keep command handlers thin.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crossbeam_channel::Receiver;
use tauri::{AppHandle, Emitter};

use crate::asr::{AsrConfig, AsrWorker};
use crate::audio::pipeline::ProcessedAudioChunk;
use crate::diarization::{
    DiarizationConfig, DiarizationInput, DiarizationWorker, DiarizedTranscript,
};
use crate::events::{self, PipelineStatus, StageStatus};
use crate::graph::entities::{ExtractionResult, GraphSnapshot};
use crate::graph::extraction::RuleBasedExtractor;
use crate::graph::temporal::TemporalKnowledgeGraph;
use crate::llm::{ApiClient, LlmEngine};
use crate::models::SORTFORMER_MODEL_FILENAME;
use crate::settings::{AsrProvider, LlmProvider};
use crate::state::TranscriptSegment;

// ---------------------------------------------------------------------------
// Accumulated speech segment (replaces the old VAD-produced SpeechSegment)
// ---------------------------------------------------------------------------

/// A segment of speech audio accumulated from the processed audio pipeline.
///
/// The speech processor accumulates `ProcessedAudioChunk`s into ~2 second
/// segments for better Whisper transcription quality (individual 32ms chunks
/// are too short for coherent speech recognition).
#[derive(Debug, Clone)]
pub(crate) struct AccumulatedSegment {
    /// Identifier of the audio source that produced this segment.
    pub source_id: String,
    /// 16kHz mono f32 audio data for the segment.
    pub audio: Vec<f32>,
    /// Start time relative to stream start.
    pub start_time: Duration,
    /// End time relative to stream start.
    pub end_time: Duration,
    /// Number of audio frames (equal to `audio.len()`).
    pub num_frames: usize,
}

/// Target number of frames per accumulated segment (~2 seconds at 16kHz).
const TARGET_FRAMES: usize = 16_000 * 2;

// ---------------------------------------------------------------------------
// Diarization config helper
// ---------------------------------------------------------------------------

/// Build the best available `DiarizationConfig` for the given models directory.
///
/// If the Sortformer ONNX model file exists on disk **and** the `diarization`
/// feature is compiled in, returns a config using the Sortformer backend.
/// Otherwise falls back to the Simple signal-based backend.
fn make_diarization_config(models_dir: &std::path::Path) -> DiarizationConfig {
    let sortformer_path = models_dir.join(SORTFORMER_MODEL_FILENAME);

    if sortformer_path.exists() {
        log::info!(
            "Sortformer model found at '{}' — using neural diarization backend",
            sortformer_path.display()
        );
        DiarizationConfig::sortformer(sortformer_path)
    } else {
        log::info!(
            "Sortformer model not found at '{}' — using Simple diarization backend. \
             Download via Settings → Models for improved speaker identification.",
            sortformer_path.display()
        );
        DiarizationConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Extraction helpers
// ---------------------------------------------------------------------------

/// Try entity extraction using the native LLM engine.
/// Returns `None` if no engine is loaded or extraction fails.
fn try_native_llm(
    text: &str,
    speaker: &str,
    llm_engine: &Arc<Mutex<Option<LlmEngine>>>,
) -> Option<ExtractionResult> {
    let engine_guard = llm_engine.lock().unwrap_or_else(|e| {
        log::warn!("LLM engine mutex poisoned, recovering: {}", e);
        e.into_inner()
    });
    if let Some(ref engine) = *engine_guard {
        match engine.extract_entities(text, speaker) {
            Ok(result) => {
                log::debug!(
                    "Native LLM extraction: {} entities, {} relations",
                    result.entities.len(),
                    result.relations.len()
                );
                Some(result)
            }
            Err(e) => {
                log::warn!("Native LLM extraction failed: {}", e);
                None
            }
        }
    } else {
        None
    }
}

/// Try entity extraction using the API client.
/// Returns `None` if no client is configured or extraction fails.
fn try_api_client(
    text: &str,
    speaker: &str,
    api_client: &Arc<Mutex<Option<ApiClient>>>,
) -> Option<ExtractionResult> {
    let api_guard = api_client.lock().unwrap_or_else(|e| {
        log::warn!("API client mutex poisoned, recovering: {}", e);
        e.into_inner()
    });
    if let Some(ref client) = *api_guard {
        match client.extract_entities(text, speaker) {
            Ok(result) => {
                log::debug!(
                    "API extraction: {} entities, {} relations",
                    result.entities.len(),
                    result.relations.len()
                );
                Some(result)
            }
            Err(e) => {
                log::warn!("API extraction failed: {}", e);
                None
            }
        }
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Helper: extraction + graph update + event emission (I1: deduplicated)
// ---------------------------------------------------------------------------

/// Perform entity extraction, update the knowledge graph, and emit events.
///
/// Shared by both the full (ASR + diarization) and diarization-only speech
/// processor loops.  Extraction is routed based on the user's `LlmProvider`
/// preference, with automatic fallback:
///   `LocalLlama` → native LLM → API → rule-based
///   `Api`        → API → native LLM → rule-based
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_extraction_and_emit(
    text: &str,
    speaker: &str,
    segment_id: &str,
    timestamp: f64,
    llm_engine: &Arc<Mutex<Option<LlmEngine>>>,
    api_client: &Arc<Mutex<Option<ApiClient>>>,
    llm_provider: &LlmProvider,
    graph_extractor: &Arc<RuleBasedExtractor>,
    knowledge_graph: &Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: &Arc<RwLock<GraphSnapshot>>,
    pipeline_status: &Arc<RwLock<PipelineStatus>>,
    app_handle: &AppHandle,
    extraction_count: &mut u64,
    graph_update_count: &mut u64,
) {
    // Route extraction based on user's LLM provider preference
    let extraction_result = match llm_provider {
        LlmProvider::LocalLlama => {
            // Prefer native LLM → fallback to API → fallback to rule-based
            try_native_llm(text, speaker, llm_engine)
                .or_else(|| try_api_client(text, speaker, api_client))
                .unwrap_or_else(|| graph_extractor.extract(speaker, text))
        }
        LlmProvider::Api { .. } => {
            // Prefer API → fallback to native LLM → fallback to rule-based
            try_api_client(text, speaker, api_client)
                .or_else(|| try_native_llm(text, speaker, llm_engine))
                .unwrap_or_else(|| graph_extractor.extract(speaker, text))
        }
    };

    *extraction_count += 1;

    // Feed extraction into the knowledge graph
    if !extraction_result.entities.is_empty() {
        let mut graph = knowledge_graph.lock().unwrap_or_else(|e| {
            log::warn!("Knowledge graph mutex poisoned, recovering: {}", e);
            e.into_inner()
        });
        graph.process_extraction(&extraction_result, timestamp, speaker, segment_id);

        *graph_update_count += 1;

        // Update graph snapshot for frontend
        let snapshot = graph.snapshot();
        if let Ok(mut gs) = graph_snapshot.write() {
            *gs = snapshot.clone();
        }

        // Emit graph-update event
        let _ = app_handle.emit(crate::events::GRAPH_UPDATE, &snapshot);

        log::debug!(
            "Graph updated: {} nodes, {} edges",
            snapshot.stats.total_nodes,
            snapshot.stats.total_edges
        );
    }

    // Update entity_extraction and graph status, then emit pipeline status
    if let Ok(mut status) = pipeline_status.write() {
        status.entity_extraction = StageStatus::Running {
            processed_count: *extraction_count,
        };
        status.graph = StageStatus::Running {
            processed_count: *graph_update_count,
        };
    }
    if let Ok(status) = pipeline_status.read() {
        let _ = app_handle.emit(events::PIPELINE_STATUS_EVENT, &*status);
    }
}

// ---------------------------------------------------------------------------
// Audio accumulation helper
// ---------------------------------------------------------------------------

/// Accumulator that collects `ProcessedAudioChunk`s into `AccumulatedSegment`s
/// of approximately `TARGET_FRAMES` length.
struct AudioAccumulator {
    audio: Vec<f32>,
    source_id: String,
    segment_start: Option<Duration>,
    segment_end: Duration,
}

impl AudioAccumulator {
    fn new() -> Self {
        Self {
            audio: Vec::with_capacity(TARGET_FRAMES),
            source_id: String::new(),
            segment_start: None,
            segment_end: Duration::ZERO,
        }
    }

    /// Feed a chunk. Returns `Some(AccumulatedSegment)` if the accumulator
    /// has reached the target size, otherwise `None`.
    fn feed(&mut self, chunk: &ProcessedAudioChunk) -> Option<AccumulatedSegment> {
        if self.source_id.is_empty() {
            self.source_id = chunk.source_id.clone();
        }
        if self.segment_start.is_none() {
            self.segment_start = chunk.timestamp;
        }
        self.segment_end = chunk.timestamp.unwrap_or(Duration::ZERO);
        self.audio.extend_from_slice(&chunk.data);

        if self.audio.len() >= TARGET_FRAMES {
            Some(self.take())
        } else {
            None
        }
    }

    /// Take the current accumulated audio as a segment and reset.
    fn take(&mut self) -> AccumulatedSegment {
        let audio = std::mem::replace(&mut self.audio, Vec::with_capacity(TARGET_FRAMES));
        let num_frames = audio.len();
        let seg = AccumulatedSegment {
            source_id: self.source_id.clone(),
            audio,
            start_time: self.segment_start.unwrap_or(Duration::ZERO),
            end_time: self.segment_end,
            num_frames,
        };
        self.segment_start = None;
        seg
    }

    /// Flush any remaining audio as a final segment. Returns `None` if empty.
    fn flush(mut self) -> Option<AccumulatedSegment> {
        if self.audio.is_empty() {
            None
        } else {
            Some(self.take())
        }
    }
}

// ---------------------------------------------------------------------------
// Speech processor threads
// ---------------------------------------------------------------------------

/// Speech processor orchestrator — runs ASR and diarization inline on a
/// single thread. Receives `ProcessedAudioChunk`s from the pipeline,
/// accumulates them into ~2s segments, transcribes each via Whisper,
/// diarizes, then emits Tauri events and stores results.
pub(crate) fn run_speech_processor(
    processed_rx: Receiver<ProcessedAudioChunk>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    models_dir: PathBuf,
    asr_provider: AsrProvider,
    llm_provider: LlmProvider,
) {
    use whisper_rs::{WhisperContext, WhisperContextParameters};

    // Macro to reduce duplication: each fallback site calls
    // run_speech_processor_diarization_only with the same arguments
    // and then returns.  Only one branch is ever taken at runtime, so
    // the compiler accepts the conditional moves.
    macro_rules! fallback_diarization_only {
        () => {
            run_speech_processor_diarization_only(
                processed_rx,
                is_transcribing,
                transcript_buffer,
                pipeline_status,
                app_handle,
                knowledge_graph,
                graph_snapshot,
                graph_extractor,
                llm_engine,
                api_client,
                models_dir,
                llm_provider,
            );
            return;
        };
    }

    // Log LLM provider for diagnostics
    match &llm_provider {
        LlmProvider::LocalLlama => {
            log::info!("Speech processor: LLM provider is LocalLlama — will prefer native LLM engine for entity extraction.");
        }
        LlmProvider::Api {
            endpoint, model, ..
        } => {
            log::info!(
                "Speech processor: LLM provider is API (endpoint={}, model={}) — will prefer API client for entity extraction.",
                endpoint,
                model
            );
        }
    }

    // ── Respect AsrProvider setting ──────────────────────────────────────
    // If the user has selected an API provider for ASR, skip local Whisper
    // model loading entirely and run in diarization-only mode.
    if matches!(asr_provider, AsrProvider::Api { .. }) {
        log::info!(
            "Speech processor: ASR provider is API — skipping local Whisper model, \
             running diarization-only mode."
        );
        fallback_diarization_only!();
    }

    log::info!("Speech processor: loading Whisper model...");

    let asr_config = AsrConfig::with_models_dir(&models_dir);
    let model_path_str = asr_config.model_path.display().to_string();

    // ── Pre-validate model file ─────────────────────────────────────────
    // Guard against missing or corrupted model files BEFORE calling into
    // whisper.cpp's C code.  In debug builds, passing a missing/truncated
    // file to whisper.cpp triggers a UCRT debug assertion crash
    // (`_osfile(fh) & FOPEN` in read.cpp:381) because the C runtime tries
    // to `read()` from an invalid file descriptor.  By checking here we
    // gracefully fall back to diarization-only mode instead of aborting.
    {
        let model_path = &asr_config.model_path;
        if !model_path.exists() {
            log::warn!(
                "Speech processor: Whisper model not found at '{}'. \
                 ASR disabled — running diarization-only mode. \
                 Download the model via Settings → Models.",
                model_path_str
            );
            fallback_diarization_only!();
        }

        match std::fs::metadata(model_path) {
            Ok(meta) => {
                // The smallest valid Whisper model (tiny) is ~75 MB.
                // Anything under 1 MB is definitely a partial download or corrupt.
                const MIN_MODEL_SIZE: u64 = 1_000_000;
                if meta.len() < MIN_MODEL_SIZE {
                    log::warn!(
                        "Speech processor: Whisper model at '{}' appears corrupted \
                         (size: {} bytes, expected >= {} bytes). \
                         ASR disabled — running diarization-only mode. \
                         Re-download the model via Settings → Models.",
                        model_path_str,
                        meta.len(),
                        MIN_MODEL_SIZE
                    );
                    fallback_diarization_only!();
                }
                log::info!(
                    "Speech processor: model file validated — {} ({:.1} MB)",
                    model_path_str,
                    meta.len() as f64 / 1_048_576.0
                );
            }
            Err(e) => {
                log::warn!(
                    "Speech processor: cannot read model file metadata at '{}': {}. \
                     ASR disabled — running diarization-only mode.",
                    model_path_str,
                    e
                );
                fallback_diarization_only!();
            }
        }
    }

    // Load Whisper model — must stay on this thread
    let ctx =
        match WhisperContext::new_with_params(&model_path_str, WhisperContextParameters::default())
        {
            Ok(ctx) => {
                log::info!(
                    "Speech processor: Whisper model loaded from {}",
                    model_path_str
                );
                ctx
            }
            Err(e) => {
                log::error!(
                    "Speech processor: failed to load Whisper model from {}: {}. \
                 ASR disabled — will still run diarization on speech segments.",
                    model_path_str,
                    e
                );
                fallback_diarization_only!();
            }
        };

    let mut whisper_state = match ctx.create_state() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Speech processor: failed to create Whisper state: {}", e);
            fallback_diarization_only!();
        }
    };

    // Create ASR worker with a dummy output channel — we call
    // `transcribe_segment()` directly rather than using the worker's
    // internal run loop, so the channel is never consumed.  This is a
    // stop-gap until `AsrWorker` gains a `new_standalone()` constructor
    // that doesn't require a channel.  (M2)
    let (dummy_asr_tx, _dummy_asr_rx) = crossbeam_channel::unbounded::<TranscriptSegment>();
    let mut asr_worker = AsrWorker::new(asr_config, dummy_asr_tx);

    // Same pattern for DiarizationWorker — `process_input()` is called
    // directly; the channel output is unused.
    // Auto-detect Sortformer model; falls back to Simple if not available.
    let diarization_config = make_diarization_config(&models_dir);
    let (dummy_diar_tx, _dummy_diar_rx) = crossbeam_channel::unbounded::<DiarizedTranscript>();
    let mut diarization_worker = DiarizationWorker::new(diarization_config, dummy_diar_tx);

    let mut asr_count: u64 = 0;
    let mut diarization_count: u64 = 0;
    let mut extraction_count: u64 = 0;
    let mut graph_update_count: u64 = 0;

    log::info!("Speech processor: entering processing loop (ASR + diarization)");

    let mut accumulator = AudioAccumulator::new();

    loop {
        // Bug 2 fix: use recv_timeout so we periodically check the stop flag
        let chunk = match processed_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(chunk) => chunk,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    log::info!("Speech processor: is_transcribing flag cleared, exiting");
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        // Also check the flag on each received chunk for faster exit
        if !is_transcribing.load(Ordering::Relaxed) {
            log::info!("Speech processor: is_transcribing flag cleared, exiting");
            break;
        }

        // Accumulate chunks into ~2s segments
        let segment = match accumulator.feed(&chunk) {
            Some(seg) => seg,
            None => continue,
        };

        // 1. Run ASR transcription
        let speech_segment = AccumulatedSegment::to_asr_segment(&segment);
        match asr_worker.transcribe_segment(&mut whisper_state, &speech_segment) {
            Ok(transcripts) => {
                for transcript in transcripts {
                    asr_count += 1;

                    // 2. Run diarization
                    let input = DiarizationInput {
                        transcript,
                        speech_audio: segment.audio.clone(),
                        speech_start_time: segment.start_time,
                        speech_end_time: segment.end_time,
                    };
                    let diarized = diarization_worker.process_input(input);
                    diarization_count += 1;

                    // 3. Store in transcript buffer
                    if let Ok(mut buffer) = transcript_buffer.write() {
                        buffer.push_back(diarized.segment.clone());
                        if buffer.len() > 500 {
                            buffer.pop_front();
                        }
                    }

                    // 4. Emit Tauri events
                    let _ = app_handle.emit(events::TRANSCRIPT_UPDATE, &diarized.segment);
                    let _ = app_handle.emit(events::SPEAKER_DETECTED, &diarized.speaker_info);

                    // 5. Update pipeline status counts
                    if let Ok(mut status) = pipeline_status.write() {
                        status.asr = StageStatus::Running {
                            processed_count: asr_count,
                        };
                        status.diarization = StageStatus::Running {
                            processed_count: diarization_count,
                        };
                    }

                    log::debug!(
                        "Speech processor: emitted transcript #{} speaker={:?} \"{}\"",
                        asr_count,
                        diarized.segment.speaker_label,
                        &diarized.segment.text,
                    );

                    // 6. Knowledge Graph Extraction (delegated to helper)
                    {
                        let speaker = diarized
                            .segment
                            .speaker_label
                            .as_deref()
                            .unwrap_or("Unknown");
                        process_extraction_and_emit(
                            &diarized.segment.text,
                            speaker,
                            &diarized.segment.id,
                            diarized.segment.start_time,
                            &llm_engine,
                            &api_client,
                            &llm_provider,
                            &graph_extractor,
                            &knowledge_graph,
                            &graph_snapshot,
                            &pipeline_status,
                            &app_handle,
                            &mut extraction_count,
                            &mut graph_update_count,
                        );
                    }
                }
            }
            Err(e) => {
                log::warn!("Speech processor: ASR failed for segment: {}", e);
            }
        }
    }

    // Flush any remaining accumulated audio
    if let Some(segment) = accumulator.flush() {
        let speech_segment = AccumulatedSegment::to_asr_segment(&segment);
        if let Ok(transcripts) = asr_worker.transcribe_segment(&mut whisper_state, &speech_segment)
        {
            for transcript in transcripts {
                asr_count += 1;
                let input = DiarizationInput {
                    transcript,
                    speech_audio: segment.audio.clone(),
                    speech_start_time: segment.start_time,
                    speech_end_time: segment.end_time,
                };
                let diarized = diarization_worker.process_input(input);
                if let Ok(mut buffer) = transcript_buffer.write() {
                    buffer.push_back(diarized.segment.clone());
                    if buffer.len() > 500 {
                        buffer.pop_front();
                    }
                }
                let _ = app_handle.emit(events::TRANSCRIPT_UPDATE, &diarized.segment);
                let _ = app_handle.emit(events::SPEAKER_DETECTED, &diarized.speaker_info);
            }
        }
    }

    log::info!(
        "Speech processor: exiting. ASR segments={}, diarized={}",
        asr_count,
        diarization_count,
    );
}

/// Fallback speech processor — diarization only (no ASR).
///
/// Used when the Whisper model fails to load. Generates placeholder transcript
/// segments with `[speech]` text and still performs speaker attribution.
pub(crate) fn run_speech_processor_diarization_only(
    processed_rx: Receiver<ProcessedAudioChunk>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    models_dir: PathBuf,
    llm_provider: LlmProvider,
) {
    // Auto-detect Sortformer model; falls back to Simple if not available.
    let diarization_config = make_diarization_config(&models_dir);
    // Same dummy-channel pattern as in `run_speech_processor` — see M2
    // comment there for rationale.
    let (dummy_diar_tx, _dummy_diar_rx) = crossbeam_channel::unbounded::<DiarizedTranscript>();
    let mut diarization_worker = DiarizationWorker::new(diarization_config, dummy_diar_tx);

    let mut count: u64 = 0;
    let mut extraction_count: u64 = 0;
    let mut graph_update_count: u64 = 0;

    // Mark ASR as errored since model didn't load
    if let Ok(mut status) = pipeline_status.write() {
        status.asr = StageStatus::Error {
            message: "Whisper model not loaded".to_string(),
        };
        status.entity_extraction = StageStatus::Running { processed_count: 0 };
        status.graph = StageStatus::Running { processed_count: 0 };
    }

    log::info!("Speech processor (diarization-only): entering processing loop");

    let mut accumulator = AudioAccumulator::new();

    loop {
        // Bug 2 fix: use recv_timeout so we periodically check the stop flag
        let chunk = match processed_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(chunk) => chunk,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    log::info!("Speech processor (diarization-only): is_transcribing flag cleared, exiting");
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        // Also check flag on each chunk for faster exit
        if !is_transcribing.load(Ordering::Relaxed) {
            log::info!(
                "Speech processor (diarization-only): is_transcribing flag cleared, exiting"
            );
            break;
        }

        let segment = match accumulator.feed(&chunk) {
            Some(seg) => seg,
            None => continue,
        };

        count += 1;

        // Create a placeholder transcript segment (no ASR)
        let placeholder_transcript = TranscriptSegment {
            id: uuid::Uuid::new_v4().to_string(),
            source_id: segment.source_id.clone(),
            speaker_id: None,
            speaker_label: None,
            text: "[speech]".to_string(),
            start_time: segment.start_time.as_secs_f64(),
            end_time: segment.end_time.as_secs_f64(),
            confidence: 0.0,
        };

        let input = DiarizationInput {
            transcript: placeholder_transcript,
            speech_audio: segment.audio.clone(),
            speech_start_time: segment.start_time,
            speech_end_time: segment.end_time,
        };
        let diarized = diarization_worker.process_input(input);

        if let Ok(mut buffer) = transcript_buffer.write() {
            buffer.push_back(diarized.segment.clone());
            if buffer.len() > 500 {
                buffer.pop_front();
            }
        }

        let _ = app_handle.emit(events::TRANSCRIPT_UPDATE, &diarized.segment);
        let _ = app_handle.emit(events::SPEAKER_DETECTED, &diarized.speaker_info);

        if let Ok(mut status) = pipeline_status.write() {
            status.diarization = StageStatus::Running {
                processed_count: count,
            };
        }

        // Knowledge Graph Extraction (delegated to helper)
        {
            let speaker = diarized
                .segment
                .speaker_label
                .as_deref()
                .unwrap_or("Unknown");
            process_extraction_and_emit(
                &diarized.segment.text,
                speaker,
                &diarized.segment.id,
                diarized.segment.start_time,
                &llm_engine,
                &api_client,
                &llm_provider,
                &graph_extractor,
                &knowledge_graph,
                &graph_snapshot,
                &pipeline_status,
                &app_handle,
                &mut extraction_count,
                &mut graph_update_count,
            );
        }
    }

    log::info!(
        "Speech processor (diarization-only): exiting. Segments processed={}",
        count,
    );
}

// ---------------------------------------------------------------------------
// AccumulatedSegment → ASR bridge
// ---------------------------------------------------------------------------

impl AccumulatedSegment {
    /// Convert an `AccumulatedSegment` into the `SpeechSegment` type expected
    /// by the ASR worker.
    fn to_asr_segment(seg: &AccumulatedSegment) -> crate::asr::SpeechSegment {
        crate::asr::SpeechSegment {
            source_id: seg.source_id.clone(),
            audio: seg.audio.clone(),
            start_time: seg.start_time,
            end_time: seg.end_time,
            num_frames: seg.num_frames,
        }
    }
}
