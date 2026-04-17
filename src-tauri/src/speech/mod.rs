//! Speech processing orchestrator.
//!
//! Contains the speech processor logic (ASR + diarization + entity extraction)
//! extracted from `commands.rs` to keep command handlers thin.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

/// Bounded thread pool for fire-and-forget entity extraction tasks.
///
/// Previously, each transcript segment spawned a new `std::thread` — a 10-hour
/// session at 2 segments/sec creates 72,000 threads, exhausting OS thread
/// limits (typically 1024-4096 per process). Using rayon's work-stealing pool
/// with a fixed worker count (4) eliminates this issue while still giving
/// extraction tasks their own thread budget separate from the ASR critical path.
fn extraction_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .thread_name(|i| format!("extraction-{}", i))
            .build()
            .expect("Failed to build extraction thread pool")
    })
}

use crossbeam_channel::Receiver;
use tauri::{AppHandle, Emitter};

use crate::asr::cloud::CloudAsrConfig;
use crate::asr::{AsrConfig, AsrWorker};
use crate::audio::pipeline::ProcessedAudioChunk;
use crate::diarization::{
    DiarizationConfig, DiarizationInput, DiarizationWorker, DiarizedTranscript,
};
use crate::events::{self, PipelineStatus, StageStatus};
use crate::graph::entities::{ExtractionResult, GraphSnapshot};
use crate::graph::extraction::RuleBasedExtractor;
use crate::graph::temporal::TemporalKnowledgeGraph;
use crate::llm::{ApiClient, LlmEngine, MistralRsEngine};
use crate::models::SORTFORMER_MODEL_FILENAME;
use crate::settings::{AsrProvider, LlmProvider};
use crate::state::{SpeakerInfo, TranscriptSegment};

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

/// Number of frames to retain as overlap between consecutive segments (~0.5s at 16kHz).
/// This ensures words at segment boundaries are captured in both adjacent segments.
const OVERLAP_FRAMES: usize = 16_000 / 2;

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

/// Try entity extraction using the mistral.rs engine.
/// Returns `None` if no engine is loaded or extraction fails.
fn try_mistralrs_engine(
    text: &str,
    speaker: &str,
    mistralrs_engine: &Arc<Mutex<Option<MistralRsEngine>>>,
) -> Option<ExtractionResult> {
    let engine_guard = mistralrs_engine.lock().unwrap_or_else(|e| {
        log::warn!("mistral.rs engine mutex poisoned, recovering: {}", e);
        e.into_inner()
    });
    if let Some(ref engine) = *engine_guard {
        match engine.extract_entities(text, speaker) {
            Ok(result) => {
                log::debug!(
                    "mistral.rs extraction: {} entities, {} relations",
                    result.entities.len(),
                    result.relations.len()
                );
                Some(result)
            }
            Err(e) => {
                log::warn!("mistral.rs extraction failed: {}", e);
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
    mistralrs_engine: &Arc<Mutex<Option<MistralRsEngine>>>,
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
        LlmProvider::Api { .. } | LlmProvider::AwsBedrock { .. } => {
            // Prefer API → fallback to native LLM → fallback to rule-based
            try_api_client(text, speaker, api_client)
                .or_else(|| try_native_llm(text, speaker, llm_engine))
                .unwrap_or_else(|| graph_extractor.extract(speaker, text))
        }
        LlmProvider::MistralRs { .. } => {
            // Prefer mistral.rs → fallback to native LLM → fallback to API → rule-based
            try_mistralrs_engine(text, speaker, mistralrs_engine)
                .or_else(|| try_native_llm(text, speaker, llm_engine))
                .or_else(|| try_api_client(text, speaker, api_client))
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

        // Emit delta update (every extraction cycle — lightweight)
        if graph.has_delta() {
            let delta = graph.take_delta();
            let _ = app_handle.emit(crate::events::GRAPH_DELTA, &delta);
            log::debug!(
                "Graph delta emitted: +{} nodes, ~{} updated, +{} edges, -{} nodes, -{} edges",
                delta.added_nodes.len(),
                delta.updated_nodes.len(),
                delta.added_edges.len(),
                delta.removed_node_ids.len(),
                delta.removed_edge_ids.len(),
            );
        }

        // Emit full snapshot less frequently (every 10th update)
        if *graph_update_count % 10 == 0 {
            let snapshot = graph.snapshot();
            if let Ok(mut gs) = graph_snapshot.write() {
                *gs = snapshot.clone();
            }
            let _ = app_handle.emit(crate::events::GRAPH_UPDATE, &snapshot);
            log::debug!(
                "Graph full snapshot emitted: {} nodes, {} edges (update #{})",
                snapshot.stats.total_nodes,
                snapshot.stats.total_edges,
                graph_update_count,
            );
        } else {
            // Still update the cached snapshot (for Tauri commands that read it)
            let snapshot = graph.snapshot();
            if let Ok(mut gs) = graph_snapshot.write() {
                *gs = snapshot;
            }
        }
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
// Shared post-transcription tail pipeline
// ---------------------------------------------------------------------------

/// Shared dependencies for post-transcription processing across all ASR workers.
///
/// Every ASR worker — local Whisper, cloud batch, Deepgram/AssemblyAI/AWS
/// streaming, sherpa-onnx streaming — runs an identical tail once it has a
/// final `TranscriptSegment`: buffer + persist + emit + status + extract.
/// Collecting these dependencies in one struct lets that tail live in
/// [`emit_transcript_and_extract`] instead of being copied six times.
#[derive(Clone)]
pub(crate) struct TranscriptProcessingContext {
    pub transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    pub transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pub pipeline_status: Arc<RwLock<PipelineStatus>>,
    pub app_handle: AppHandle,
    pub llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    pub api_client: Arc<Mutex<Option<ApiClient>>>,
    pub mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    pub llm_provider: LlmProvider,
    pub graph_extractor: Arc<RuleBasedExtractor>,
    pub knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    pub graph_snapshot: Arc<RwLock<GraphSnapshot>>,
}

/// Store, emit, update status, and spawn extraction for a final transcript
/// segment. Shared by every ASR worker implementation to eliminate the
/// ~60-line tail that used to be copied inline at each call site.
///
/// Behaviour preserved exactly from the original inline copies:
/// - Append to the 500-item ring buffer, persist to disk, emit
///   `TRANSCRIPT_UPDATE`, write pipeline status, fire extraction.
/// - `speaker_info` controls the `SPEAKER_DETECTED` event: pass `Some(info)`
///   for the diarized-in-place workers (local/cloud/AWS) where speaker_info
///   was previously emitted here; pass `None` for the streaming receivers
///   (Deepgram/AssemblyAI/sherpa) where `SPEAKER_DETECTED` is already emitted
///   earlier, inside the diarization branch.
pub(crate) fn emit_transcript_and_extract(
    segment: TranscriptSegment,
    speaker_info: Option<SpeakerInfo>,
    ctx: &TranscriptProcessingContext,
    asr_count: u64,
    diarization_count: u64,
    extraction_count: &Arc<AtomicU64>,
    graph_update_count: &Arc<AtomicU64>,
) {
    // 1. Store in transcript buffer (ring-buffered at 500 items).
    if let Ok(mut buffer) = ctx.transcript_buffer.write() {
        buffer.push_back(segment.clone());
        if buffer.len() > 500 {
            buffer.pop_front();
        }
    }
    // 2. Persist transcript segment.
    if let Ok(writer_guard) = ctx.transcript_writer.lock() {
        if let Some(ref writer) = *writer_guard {
            writer.append(&segment);
        }
    }

    // 3. Emit Tauri events.
    let _ = ctx.app_handle.emit(events::TRANSCRIPT_UPDATE, &segment);
    if let Some(info) = speaker_info.as_ref() {
        let _ = ctx.app_handle.emit(events::SPEAKER_DETECTED, info);
    }

    // 4. Update pipeline status counts.
    if let Ok(mut status) = ctx.pipeline_status.write() {
        status.asr = StageStatus::Running {
            processed_count: asr_count,
        };
        status.diarization = StageStatus::Running {
            processed_count: diarization_count,
        };
    }

    // 5. Knowledge Graph Extraction — fire-and-forget.
    spawn_extraction_task(
        segment.text.clone(),
        segment
            .speaker_label
            .clone()
            .unwrap_or_else(|| "Unknown".to_string()),
        segment.id.clone(),
        segment.start_time,
        &ctx.llm_engine,
        &ctx.api_client,
        &ctx.mistralrs_engine,
        &ctx.llm_provider,
        &ctx.graph_extractor,
        &ctx.knowledge_graph,
        &ctx.graph_snapshot,
        &ctx.pipeline_status,
        &ctx.app_handle,
        extraction_count,
        graph_update_count,
    );
}

// ---------------------------------------------------------------------------
// Fire-and-forget extraction task
// ---------------------------------------------------------------------------

/// Spawn entity extraction on a separate thread so it doesn't block the
/// ASR processing loop. Falls back to inline execution if thread spawn fails.
#[allow(clippy::too_many_arguments)]
fn spawn_extraction_task(
    text: String,
    speaker: String,
    segment_id: String,
    timestamp: f64,
    llm_engine: &Arc<Mutex<Option<LlmEngine>>>,
    api_client: &Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: &Arc<Mutex<Option<MistralRsEngine>>>,
    llm_provider: &LlmProvider,
    graph_extractor: &Arc<RuleBasedExtractor>,
    knowledge_graph: &Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: &Arc<RwLock<GraphSnapshot>>,
    pipeline_status: &Arc<RwLock<PipelineStatus>>,
    app_handle: &AppHandle,
    extraction_count: &Arc<std::sync::atomic::AtomicU64>,
    graph_update_count: &Arc<std::sync::atomic::AtomicU64>,
) {
    let llm_engine = llm_engine.clone();
    let api_client = api_client.clone();
    let mistralrs_engine = mistralrs_engine.clone();
    let llm_provider = llm_provider.clone();
    let graph_extractor = graph_extractor.clone();
    let knowledge_graph = knowledge_graph.clone();
    let graph_snapshot = graph_snapshot.clone();
    let pipeline_status = pipeline_status.clone();
    let app_handle = app_handle.clone();
    let extraction_count = extraction_count.clone();
    let graph_update_count = graph_update_count.clone();

    let run_extraction = move || {
        let mut local_extraction = extraction_count.load(Ordering::Relaxed);
        let mut local_graph = graph_update_count.load(Ordering::Relaxed);
        process_extraction_and_emit(
            &text,
            &speaker,
            &segment_id,
            timestamp,
            &llm_engine,
            &api_client,
            &mistralrs_engine,
            &llm_provider,
            &graph_extractor,
            &knowledge_graph,
            &graph_snapshot,
            &pipeline_status,
            &app_handle,
            &mut local_extraction,
            &mut local_graph,
        );
        extraction_count.store(local_extraction, Ordering::Relaxed);
        graph_update_count.store(local_graph, Ordering::Relaxed);
    };

    // Submit to the bounded rayon thread pool (4 workers). Unlike
    // `std::thread::spawn`, `rayon::ThreadPool::spawn` cannot fail — work is
    // queued on an existing worker. This prevents OS thread exhaustion during
    // long sessions (previously 72K+ threads in 10hrs at 2 segments/sec).
    extraction_pool().spawn(run_extraction);
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

    /// Take the current accumulated audio as a segment, retaining the last
    /// `OVERLAP_FRAMES` samples for continuity with the next segment.
    fn take(&mut self) -> AccumulatedSegment {
        let full_audio = std::mem::replace(&mut self.audio, Vec::with_capacity(TARGET_FRAMES));
        let num_frames = full_audio.len();
        let seg_start = self.segment_start.unwrap_or(Duration::ZERO);
        let seg_end = self.segment_end;

        // Retain the last OVERLAP_FRAMES samples for the next segment
        let overlap_start = if num_frames > OVERLAP_FRAMES {
            num_frames - OVERLAP_FRAMES
        } else {
            0
        };
        self.audio.extend_from_slice(&full_audio[overlap_start..]);

        // Compute overlap duration so the next segment's start time is set correctly
        let overlap_duration =
            Duration::from_secs_f64((num_frames - overlap_start) as f64 / 16_000.0);
        // The next segment starts at (end_time - overlap_duration)
        self.segment_start = Some(seg_end.saturating_sub(overlap_duration));

        AccumulatedSegment {
            source_id: self.source_id.clone(),
            audio: full_audio,
            start_time: seg_start,
            end_time: seg_end,
            num_frames,
        }
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
// Speech processor threads (2-thread model)
// ---------------------------------------------------------------------------

/// Speech processor orchestrator — 2-thread architecture:
///
/// 1. **Accumulator thread** (this function): Receives `ProcessedAudioChunk`s,
///    accumulates them into ~2s segments, and sends them to the ASR worker.
///    Always consuming from the channel, never blocked by inference.
///
/// 2. **ASR worker thread** (spawned internally): Receives accumulated segments,
///    runs Whisper transcription, diarization, and fires off extraction.
///
/// Returns a `JoinHandle` for the spawned ASR worker thread so the caller
/// can track it for clean shutdown.
pub(crate) fn run_speech_processor(
    processed_rx: Receiver<ProcessedAudioChunk>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    models_dir: PathBuf,
    asr_provider: AsrProvider,
    llm_provider: LlmProvider,
    whisper_model: String,
) {
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
                transcript_writer,
                pipeline_status,
                app_handle,
                knowledge_graph,
                graph_snapshot,
                graph_extractor,
                llm_engine,
                api_client,
                mistralrs_engine,
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
        LlmProvider::AwsBedrock {
            region, model_id, ..
        } => {
            log::info!(
                "Speech processor: LLM provider is AWS Bedrock (region={}, model={}) — will prefer API client for entity extraction.",
                region,
                model_id
            );
        }
        LlmProvider::MistralRs { ref model_id } => {
            log::info!(
                "Speech processor: LLM provider is mistral.rs (model={}).",
                model_id
            );
        }
    }

    // ── Respect AsrProvider setting ──────────────────────────────────────
    // If the user has selected a cloud API provider for ASR, launch the
    // cloud ASR worker instead of loading local Whisper.
    if let AsrProvider::Api {
        ref endpoint,
        ref api_key,
        ref model,
    } = asr_provider
    {
        log::info!(
            "Speech processor: ASR provider is cloud API (endpoint={}, model={}) — \
             launching cloud ASR worker.",
            endpoint,
            model
        );
        let cloud_config = CloudAsrConfig {
            endpoint: endpoint.clone(),
            api_key: api_key.clone(),
            model: model.clone(),
            language: "en".to_string(),
        };
        run_cloud_asr_speech_processor(
            processed_rx,
            is_transcribing,
            transcript_buffer,
            transcript_writer,
            pipeline_status,
            app_handle,
            knowledge_graph,
            graph_snapshot,
            graph_extractor,
            llm_engine,
            api_client,
            mistralrs_engine,
            models_dir,
            llm_provider,
            cloud_config,
        );
        return;
    }

    // If the user selected Deepgram streaming ASR, launch the streaming
    // WebSocket worker instead of loading local Whisper.
    if let AsrProvider::DeepgramStreaming {
        ref api_key,
        ref model,
        enable_diarization,
    } = asr_provider
    {
        log::info!(
            "Speech processor: ASR provider is Deepgram streaming (model={}) — \
             launching Deepgram streaming worker.",
            model
        );
        let deepgram_config = crate::asr::deepgram::DeepgramConfig {
            api_key: api_key.clone(),
            model: model.clone(),
            enable_diarization,
        };
        run_deepgram_speech_processor(
            processed_rx,
            is_transcribing,
            transcript_buffer,
            transcript_writer,
            pipeline_status,
            app_handle,
            knowledge_graph,
            graph_snapshot,
            graph_extractor,
            llm_engine,
            api_client,
            mistralrs_engine,
            models_dir,
            llm_provider,
            deepgram_config,
        );
        return;
    }

    // If the user selected AssemblyAI streaming ASR, launch the streaming
    // WebSocket worker instead of loading local Whisper.
    if let AsrProvider::AssemblyAI {
        ref api_key,
        enable_diarization,
    } = asr_provider
    {
        log::info!(
            "Speech processor: ASR provider is AssemblyAI streaming — \
             launching AssemblyAI streaming worker."
        );
        let assemblyai_config = crate::asr::assemblyai::AssemblyAIConfig {
            api_key: api_key.clone(),
            enable_diarization,
        };
        run_assemblyai_speech_processor(
            processed_rx,
            is_transcribing,
            transcript_buffer,
            transcript_writer,
            pipeline_status,
            app_handle,
            knowledge_graph,
            graph_snapshot,
            graph_extractor,
            llm_engine,
            api_client,
            mistralrs_engine,
            models_dir,
            llm_provider,
            assemblyai_config,
        );
        return;
    }

    if let AsrProvider::AwsTranscribe {
        ref region,
        ref language_code,
        ref credential_source,
        enable_diarization,
    } = asr_provider
    {
        log::info!(
            "Speech processor: ASR provider is AWS Transcribe (region={}) — \
             launching streaming session.",
            region
        );
        let aws_config = crate::asr::aws_transcribe::AwsTranscribeConfig {
            region: region.clone(),
            language_code: language_code.clone(),
            credential_source: credential_source.clone(),
            enable_diarization,
        };
        run_aws_transcribe_speech_processor(
            processed_rx,
            is_transcribing,
            transcript_buffer,
            transcript_writer,
            pipeline_status,
            app_handle,
            knowledge_graph,
            graph_snapshot,
            graph_extractor,
            llm_engine,
            api_client,
            mistralrs_engine,
            models_dir,
            llm_provider,
            aws_config,
        );
        return;
    }

    // If the user selected sherpa-onnx streaming ASR, launch the streaming
    // worker that processes every audio chunk frame-by-frame.
    #[cfg(feature = "sherpa-streaming")]
    if let AsrProvider::SherpaOnnx {
        ref model_dir,
        enable_endpoint_detection,
    } = asr_provider
    {
        log::info!(
            "Speech processor: ASR provider is sherpa-onnx streaming (model_dir={}) — \
             launching streaming worker.",
            model_dir
        );
        let sherpa_config = crate::asr::sherpa_streaming::SherpaStreamingConfig {
            model_dir: models_dir.join(model_dir),
            enable_endpoint_detection,
        };
        run_sherpa_onnx_speech_processor(
            processed_rx,
            is_transcribing,
            transcript_buffer,
            transcript_writer,
            pipeline_status,
            app_handle,
            knowledge_graph,
            graph_snapshot,
            graph_extractor,
            llm_engine,
            api_client,
            mistralrs_engine,
            models_dir,
            llm_provider,
            sherpa_config,
        );
        return;
    }

    #[cfg(not(feature = "sherpa-streaming"))]
    if matches!(asr_provider, AsrProvider::SherpaOnnx { .. }) {
        log::error!(
            "Speech processor: sherpa-onnx ASR provider selected but the \
             'sherpa-streaming' feature is not enabled. Falling back to \
             diarization-only mode."
        );
        fallback_diarization_only!();
    }

    log::info!("Speech processor: loading Whisper model...");

    let asr_config = AsrConfig::with_models_dir_and_model(&models_dir, &whisper_model);
    let model_path_str = asr_config.model_path.display().to_string();

    // ── Pre-validate model file ─────────────────────────────────────────
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

    // ── Create internal channel: accumulator → ASR worker ───────────────
    // Capacity 4 = up to 8s of buffered segments; prevents unbounded growth
    // while giving the ASR worker headroom for inference latency.
    let (asr_seg_tx, asr_seg_rx) = crossbeam_channel::bounded::<AccumulatedSegment>(4);

    // ── Spawn ASR + processing worker thread ────────────────────────────
    let is_transcribing_asr = is_transcribing.clone();
    let asr_worker_handle = std::thread::Builder::new()
        .name("asr-worker".to_string())
        .spawn({
            let transcript_buffer = transcript_buffer.clone();
            let transcript_writer = transcript_writer.clone();
            let pipeline_status = pipeline_status.clone();
            let app_handle = app_handle.clone();
            let knowledge_graph = knowledge_graph.clone();
            let graph_snapshot = graph_snapshot.clone();
            let graph_extractor = graph_extractor.clone();
            let llm_engine = llm_engine.clone();
            let api_client = api_client.clone();
            let mistralrs_engine = mistralrs_engine.clone();
            let llm_provider = llm_provider.clone();
            let models_dir = models_dir.clone();
            let model_path_str = model_path_str.clone();
            let asr_config = AsrConfig::with_models_dir_and_model(&models_dir, &whisper_model);

            move || {
                run_asr_worker(
                    asr_seg_rx,
                    is_transcribing_asr,
                    transcript_buffer,
                    transcript_writer,
                    pipeline_status,
                    app_handle,
                    knowledge_graph,
                    graph_snapshot,
                    graph_extractor,
                    llm_engine,
                    api_client,
                    mistralrs_engine,
                    llm_provider,
                    models_dir,
                    model_path_str,
                    asr_config,
                );
            }
        });

    match asr_worker_handle {
        Ok(_handle) => {
            // Store handle if needed for shutdown; currently the thread exits
            // when asr_seg_tx is dropped (channel disconnect) or the stop flag.
            log::info!("ASR worker thread spawned successfully");
            // We intentionally don't join here — the accumulator thread runs
            // independently. The handle is dropped, but the thread lives on
            // until the channel disconnects.
            // Note: the caller in commands.rs can store the asr-worker thread
            // handle separately if needed.
        }
        Err(e) => {
            log::error!("Failed to spawn ASR worker thread: {}", e);
            // Fall back to diarization-only on the current thread
            fallback_diarization_only!();
        }
    }

    // ── Accumulator loop (this thread) ──────────────────────────────────
    // Lightweight: just receives chunks, accumulates, and sends segments.
    // Never blocked by ASR inference.
    log::info!("Speech processor: entering accumulator loop");
    let mut accumulator = AudioAccumulator::new();

    loop {
        let chunk = match processed_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(chunk) => chunk,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    log::info!(
                        "Speech processor (accumulator): is_transcribing flag cleared, exiting"
                    );
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        if !is_transcribing.load(Ordering::Relaxed) {
            log::info!("Speech processor (accumulator): is_transcribing flag cleared, exiting");
            break;
        }

        // Accumulate chunks into ~2s segments
        if let Some(segment) = accumulator.feed(&chunk) {
            // Send to ASR worker; if channel full, log and drop (ASR can't keep up)
            if let Err(crossbeam_channel::TrySendError::Full(seg)) = asr_seg_tx.try_send(segment) {
                log::warn!(
                    "Speech processor: ASR segment channel full, dropping {:.2}s segment \
                     (ASR inference slower than real-time)",
                    seg.num_frames as f64 / 16_000.0
                );
            }
            // Disconnected case: ASR worker died, we'll detect on next iteration
        }
    }

    // Flush remaining audio
    if let Some(segment) = accumulator.flush() {
        let _ = asr_seg_tx.try_send(segment);
    }

    // Drop the sender to signal the ASR worker to exit
    drop(asr_seg_tx);

    log::info!("Speech processor (accumulator): exiting");
}

// ---------------------------------------------------------------------------
// ASR + Processing worker (runs on dedicated thread)
// ---------------------------------------------------------------------------

/// ASR worker thread: receives accumulated segments, runs Whisper transcription,
/// diarization, stores results, emits events, and fires off extraction as
/// fire-and-forget tasks to avoid blocking the processing loop.
fn run_asr_worker(
    asr_seg_rx: Receiver<AccumulatedSegment>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    llm_provider: LlmProvider,
    models_dir: PathBuf,
    model_path_str: String,
    asr_config: AsrConfig,
) {
    use whisper_rs::{WhisperContext, WhisperContextParameters};

    // ── Load Whisper model on this thread ────────────────────────────────
    let ctx =
        match WhisperContext::new_with_params(&model_path_str, WhisperContextParameters::default())
        {
            Ok(ctx) => {
                log::info!("ASR worker: Whisper model loaded from {}", model_path_str);
                ctx
            }
            Err(e) => {
                log::error!(
                    "ASR worker: failed to load Whisper model from {}: {}. Exiting.",
                    model_path_str,
                    e
                );
                return;
            }
        };

    let mut whisper_state = match ctx.create_state() {
        Ok(s) => s,
        Err(e) => {
            log::error!("ASR worker: failed to create Whisper state: {}", e);
            return;
        }
    };

    let (dummy_asr_tx, _dummy_asr_rx) = crossbeam_channel::unbounded::<TranscriptSegment>();
    let mut asr_worker = AsrWorker::new(asr_config, dummy_asr_tx);

    let diarization_config = make_diarization_config(&models_dir);
    let (dummy_diar_tx, _dummy_diar_rx) = crossbeam_channel::unbounded::<DiarizedTranscript>();
    let mut diarization_worker = DiarizationWorker::new(diarization_config, dummy_diar_tx);

    let mut asr_count: u64 = 0;
    let mut diarization_count: u64 = 0;
    // Extraction counts are tracked via Arc<AtomicU64> shared with fire-and-forget threads
    let extraction_count = Arc::new(AtomicU64::new(0));
    let graph_update_count = Arc::new(AtomicU64::new(0));

    let ctx = TranscriptProcessingContext {
        transcript_buffer,
        transcript_writer,
        pipeline_status,
        app_handle,
        llm_engine,
        api_client,
        mistralrs_engine,
        llm_provider,
        graph_extractor,
        knowledge_graph,
        graph_snapshot,
    };

    log::info!("ASR worker: entering processing loop");

    loop {
        let segment = match asr_seg_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(seg) => seg,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    log::info!("ASR worker: is_transcribing flag cleared, exiting");
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                log::info!("ASR worker: segment channel disconnected, exiting");
                break;
            }
        };

        if !is_transcribing.load(Ordering::Relaxed) {
            log::info!("ASR worker: is_transcribing flag cleared, exiting");
            break;
        }

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

                    log::debug!(
                        "ASR worker: emitted transcript #{} speaker={:?} \"{}\"",
                        asr_count,
                        diarized.segment.speaker_label,
                        &diarized.segment.text,
                    );

                    // 3–6. Buffer, persist, emit, update status, and spawn
                    //      extraction in the shared tail helper.
                    emit_transcript_and_extract(
                        diarized.segment,
                        Some(diarized.speaker_info),
                        &ctx,
                        asr_count,
                        diarization_count,
                        &extraction_count,
                        &graph_update_count,
                    );
                }
            }
            Err(e) => {
                log::warn!("ASR worker: transcription failed for segment: {}", e);
            }
        }
    }

    log::info!(
        "ASR worker: exiting. ASR segments={}, diarized={}",
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
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
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
    let extraction_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let graph_update_count = Arc::new(std::sync::atomic::AtomicU64::new(0));

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
        // Persist transcript segment asynchronously
        if let Ok(writer_guard) = transcript_writer.lock() {
            if let Some(ref writer) = *writer_guard {
                writer.append(&diarized.segment);
            }
        }

        let _ = app_handle.emit(events::TRANSCRIPT_UPDATE, &diarized.segment);
        let _ = app_handle.emit(events::SPEAKER_DETECTED, &diarized.speaker_info);

        if let Ok(mut status) = pipeline_status.write() {
            status.diarization = StageStatus::Running {
                processed_count: count,
            };
        }

        // Knowledge Graph Extraction — fire-and-forget
        spawn_extraction_task(
            diarized.segment.text.clone(),
            diarized
                .segment
                .speaker_label
                .clone()
                .unwrap_or_else(|| "Unknown".to_string()),
            diarized.segment.id.clone(),
            diarized.segment.start_time,
            &llm_engine,
            &api_client,
            &mistralrs_engine,
            &llm_provider,
            &graph_extractor,
            &knowledge_graph,
            &graph_snapshot,
            &pipeline_status,
            &app_handle,
            &extraction_count,
            &graph_update_count,
        );
    }

    log::info!(
        "Speech processor (diarization-only): exiting. Segments processed={}",
        count,
    );
}

// ---------------------------------------------------------------------------
// Cloud ASR speech processor (batch HTTP API)
// ---------------------------------------------------------------------------

/// Cloud ASR speech processor — same 2-thread architecture as the local
/// Whisper path, but the ASR worker sends accumulated segments to a cloud
/// STT API (OpenAI-compatible: Groq, OpenAI, Deepgram REST, etc.)
/// instead of running local inference.
pub(crate) fn run_cloud_asr_speech_processor(
    processed_rx: Receiver<ProcessedAudioChunk>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    models_dir: PathBuf,
    llm_provider: LlmProvider,
    cloud_config: CloudAsrConfig,
) {
    // Capacity 32 = up to ~64s of buffered 2s segments. Cloud ASR HTTP calls
    // can take 1–5s per segment; a short 4-slot queue overflows during
    // latency spikes and drops real audio. 32 slots give the accumulator
    // meaningful headroom while still bounding memory (~32 × 2s × 16kHz × 4B
    // ≈ 4 MB worst case).
    let (asr_seg_tx, asr_seg_rx) = crossbeam_channel::bounded::<AccumulatedSegment>(32);

    let is_transcribing_asr = is_transcribing.clone();
    let _asr_worker_handle = std::thread::Builder::new()
        .name("cloud-asr-worker".to_string())
        .spawn({
            let transcript_buffer = transcript_buffer.clone();
            let transcript_writer = transcript_writer.clone();
            let pipeline_status = pipeline_status.clone();
            let app_handle = app_handle.clone();
            let knowledge_graph = knowledge_graph.clone();
            let graph_snapshot = graph_snapshot.clone();
            let graph_extractor = graph_extractor.clone();
            let llm_engine = llm_engine.clone();
            let api_client = api_client.clone();
            let mistralrs_engine = mistralrs_engine.clone();
            let llm_provider = llm_provider.clone();
            let models_dir = models_dir.clone();

            move || {
                run_cloud_asr_worker(
                    asr_seg_rx,
                    is_transcribing_asr,
                    transcript_buffer,
                    transcript_writer,
                    pipeline_status,
                    app_handle,
                    knowledge_graph,
                    graph_snapshot,
                    graph_extractor,
                    llm_engine,
                    api_client,
                    mistralrs_engine,
                    llm_provider,
                    models_dir,
                    cloud_config,
                );
            }
        });

    if let Ok(mut status) = pipeline_status.write() {
        status.asr = StageStatus::Running { processed_count: 0 };
    }

    log::info!("Cloud ASR speech processor: entering accumulator loop");
    let mut accumulator = AudioAccumulator::new();

    loop {
        let chunk = match processed_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(chunk) => chunk,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        if !is_transcribing.load(Ordering::Relaxed) {
            break;
        }

        if let Some(segment) = accumulator.feed(&chunk) {
            if let Err(crossbeam_channel::TrySendError::Full(seg)) = asr_seg_tx.try_send(segment) {
                log::warn!(
                    "Cloud ASR: segment channel full, dropping {:.2}s segment (API slower than real-time)",
                    seg.num_frames as f64 / 16_000.0
                );
            }
        }
    }

    if let Some(final_seg) = accumulator.flush() {
        let _ = asr_seg_tx.try_send(final_seg);
    }
    drop(asr_seg_tx);

    log::info!("Cloud ASR speech processor: accumulator loop exited");
}

/// Cloud ASR worker thread — receives accumulated segments, transcribes via
/// HTTP API, then runs the same diarization + extraction pipeline as local.
fn run_cloud_asr_worker(
    asr_seg_rx: Receiver<AccumulatedSegment>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    llm_provider: LlmProvider,
    models_dir: PathBuf,
    cloud_config: CloudAsrConfig,
) {
    let diarization_config = make_diarization_config(&models_dir);
    let (dummy_diar_tx, _dummy_diar_rx) = crossbeam_channel::unbounded::<DiarizedTranscript>();
    let mut diarization_worker = DiarizationWorker::new(diarization_config, dummy_diar_tx);

    let mut asr_count: u64 = 0;
    let mut diarization_count: u64 = 0;
    let extraction_count = Arc::new(AtomicU64::new(0));
    let graph_update_count = Arc::new(AtomicU64::new(0));

    let ctx = TranscriptProcessingContext {
        transcript_buffer,
        transcript_writer,
        pipeline_status,
        app_handle,
        llm_engine,
        api_client,
        mistralrs_engine,
        llm_provider,
        graph_extractor,
        knowledge_graph,
        graph_snapshot,
    };

    log::info!(
        "Cloud ASR worker: entering processing loop (endpoint={}, model={})",
        cloud_config.endpoint,
        cloud_config.model
    );

    loop {
        let segment = match asr_seg_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(seg) => seg,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        if !is_transcribing.load(Ordering::Relaxed) {
            break;
        }

        let speech_segment = AccumulatedSegment::to_asr_segment(&segment);
        match crate::asr::cloud::transcribe_segment(&cloud_config, &speech_segment) {
            Ok(transcripts) => {
                for transcript in transcripts {
                    asr_count += 1;

                    let input = DiarizationInput {
                        transcript,
                        speech_audio: segment.audio.clone(),
                        speech_start_time: segment.start_time,
                        speech_end_time: segment.end_time,
                    };
                    let diarized = diarization_worker.process_input(input);
                    diarization_count += 1;

                    log::debug!(
                        "Cloud ASR worker: emitted transcript #{} speaker={:?} \"{}\"",
                        asr_count,
                        diarized.segment.speaker_label,
                        &diarized.segment.text,
                    );

                    emit_transcript_and_extract(
                        diarized.segment,
                        Some(diarized.speaker_info),
                        &ctx,
                        asr_count,
                        diarization_count,
                        &extraction_count,
                        &graph_update_count,
                    );
                }
            }
            Err(e) => {
                log::warn!("Cloud ASR worker: transcription failed: {}", e);
                if let Ok(mut status) = ctx.pipeline_status.write() {
                    status.asr = StageStatus::Error {
                        message: format!("Cloud ASR error: {}", e),
                    };
                }
            }
        }
    }

    log::info!(
        "Cloud ASR worker: exiting. ASR segments={}, diarized={}",
        asr_count,
        diarization_count,
    );
}

// ---------------------------------------------------------------------------
// Deepgram Streaming ASR speech processor
// ---------------------------------------------------------------------------

/// Deepgram streaming speech processor — no accumulation needed.
///
/// Unlike batch ASR (local Whisper or cloud HTTP), Deepgram streaming receives
/// audio chunks directly and returns transcript results over the WebSocket.
/// This function:
/// 1. Creates a `DeepgramStreamingClient` and connects.
/// 2. Reads `ProcessedAudioChunk`s directly from the processed channel.
/// 3. Sends raw audio to Deepgram via `send_audio()`.
/// 4. Spawns a receiver thread that consumes Deepgram events, wraps final
///    transcripts as `TranscriptSegment`s, and feeds them through the
///    diarization + storage + events + extraction pipeline.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_deepgram_speech_processor(
    processed_rx: Receiver<ProcessedAudioChunk>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    models_dir: PathBuf,
    llm_provider: LlmProvider,
    deepgram_config: crate::asr::deepgram::DeepgramConfig,
) {
    use crate::asr::deepgram::DeepgramStreamingClient;

    // Create and connect the Deepgram client.
    let mut client = DeepgramStreamingClient::new(deepgram_config);
    match client.connect() {
        Ok(()) => {
            log::info!("Deepgram streaming: connected successfully");
        }
        Err(e) => {
            log::error!("Deepgram streaming: failed to connect: {e}");
            if let Ok(mut status) = pipeline_status.write() {
                status.asr = StageStatus::Error {
                    message: format!("Deepgram connect failed: {e}"),
                };
            }
            return;
        }
    }

    let event_rx = client.event_rx();

    // Spawn the Deepgram event receiver thread (processes transcript results).
    let is_transcribing_rx = is_transcribing.clone();
    let _receiver_handle = std::thread::Builder::new()
        .name("deepgram-event-rx".to_string())
        .spawn({
            let transcript_buffer = transcript_buffer.clone();
            let transcript_writer = transcript_writer.clone();
            let pipeline_status = pipeline_status.clone();
            let app_handle = app_handle.clone();
            let knowledge_graph = knowledge_graph.clone();
            let graph_snapshot = graph_snapshot.clone();
            let graph_extractor = graph_extractor.clone();
            let llm_engine = llm_engine.clone();
            let api_client = api_client.clone();
            let mistralrs_engine = mistralrs_engine.clone();
            let llm_provider = llm_provider.clone();
            let models_dir = models_dir.clone();

            move || {
                run_deepgram_event_receiver(
                    event_rx,
                    is_transcribing_rx,
                    transcript_buffer,
                    transcript_writer,
                    pipeline_status,
                    app_handle,
                    knowledge_graph,
                    graph_snapshot,
                    graph_extractor,
                    llm_engine,
                    api_client,
                    mistralrs_engine,
                    models_dir,
                    llm_provider,
                );
            }
        });

    // Mark ASR as running.
    if let Ok(mut status) = pipeline_status.write() {
        status.asr = StageStatus::Running { processed_count: 0 };
    }

    // Audio sender loop: reads chunks and forwards to Deepgram.
    log::info!("Deepgram streaming: entering audio sender loop");
    let mut chunks_sent: u64 = 0;

    loop {
        let chunk = match processed_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(chunk) => chunk,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    log::info!("Deepgram streaming: is_transcribing flag cleared, exiting sender");
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                log::info!("Deepgram streaming: audio channel disconnected, exiting sender");
                break;
            }
        };

        if !is_transcribing.load(Ordering::Relaxed) {
            log::info!("Deepgram streaming: is_transcribing flag cleared, exiting sender");
            break;
        }

        // NOTE: intentionally no longer checks `client.is_connected()` here.
        // The client's internal session task handles transient reconnects
        // with exponential backoff, and `send_audio` buffers into the
        // unbounded audio channel during the reconnect window. The channel
        // is only closed when the session task permanently exits (reconnect
        // budget exhausted or user-initiated disconnect), at which point the
        // `send_audio` call below will return "Audio channel closed" and we
        // fall through to the `break`.

        // Send audio directly to Deepgram (no accumulation needed).
        if let Err(e) = client.send_audio(&chunk.data) {
            log::warn!("Deepgram streaming: failed to send audio: {e}");
            break;
        }

        chunks_sent += 1;
        if chunks_sent % 100 == 0 {
            log::debug!("Deepgram streaming: sent {} audio chunks", chunks_sent);
        }
    }

    // Disconnect the client.
    client.disconnect();

    log::info!(
        "Deepgram streaming: audio sender exiting. Chunks sent={}",
        chunks_sent
    );
}

/// Deepgram event receiver thread — processes transcript events from the
/// Deepgram WebSocket and feeds them into the diarization + storage + events
/// + extraction pipeline (same downstream path as cloud ASR).
#[allow(clippy::too_many_arguments)]
fn run_deepgram_event_receiver(
    event_rx: crossbeam_channel::Receiver<crate::asr::deepgram::DeepgramEvent>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    models_dir: PathBuf,
    llm_provider: LlmProvider,
) {
    use crate::asr::deepgram::DeepgramEvent;
    use crate::diarization::{DiarizationInput, DiarizationWorker, DiarizedTranscript};

    let diarization_config = make_diarization_config(&models_dir);
    let (dummy_diar_tx, _dummy_diar_rx) = crossbeam_channel::unbounded::<DiarizedTranscript>();
    let mut diarization_worker = DiarizationWorker::new(diarization_config, dummy_diar_tx);

    let mut asr_count: u64 = 0;
    let mut diarization_count: u64 = 0;
    let extraction_count = Arc::new(AtomicU64::new(0));
    let graph_update_count = Arc::new(AtomicU64::new(0));

    let ctx = TranscriptProcessingContext {
        transcript_buffer,
        transcript_writer,
        pipeline_status,
        app_handle,
        llm_engine,
        api_client,
        mistralrs_engine,
        llm_provider,
        graph_extractor,
        knowledge_graph,
        graph_snapshot,
    };

    log::info!("Deepgram event receiver: entering processing loop");

    loop {
        let event = match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(ev) => ev,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    log::info!("Deepgram event receiver: is_transcribing flag cleared, exiting");
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                log::info!("Deepgram event receiver: event channel disconnected, exiting");
                break;
            }
        };

        if !is_transcribing.load(Ordering::Relaxed) {
            log::info!("Deepgram event receiver: is_transcribing flag cleared, exiting");
            break;
        }

        match event {
            DeepgramEvent::Transcript {
                text,
                confidence,
                is_final,
                speech_final: _,
                start,
                duration,
                words,
            } => {
                // Only process final transcripts to avoid duplicates.
                if !is_final {
                    log::debug!("Deepgram: interim transcript: \"{}\"", &text);
                    continue;
                }

                asr_count += 1;
                let end_time = start + duration;

                // Determine speaker from word-level diarization if available.
                let speaker_from_deepgram = words
                    .first()
                    .and_then(|w| w.speaker)
                    .map(|s| format!("Speaker {}", s));

                let segment = TranscriptSegment {
                    id: uuid::Uuid::new_v4().to_string(),
                    source_id: "deepgram-stream".to_string(),
                    speaker_id: speaker_from_deepgram.clone(),
                    speaker_label: speaker_from_deepgram,
                    text: text.clone(),
                    start_time: start,
                    end_time,
                    confidence,
                };

                // If Deepgram provides speaker labels, use them directly.
                // Otherwise, run through local diarization (needs audio, which
                // we don't have in the event path — so we skip diarization
                // and use the segment as-is).
                let final_segment = if segment.speaker_label.is_some() {
                    // Deepgram diarization provided speaker labels.
                    diarization_count += 1;
                    segment.clone()
                } else {
                    // No speaker from Deepgram; create a minimal diarization input
                    // with empty audio (the Simple diarization backend will
                    // assign a speaker based on signal heuristics, but with
                    // empty audio it will just assign a default speaker).
                    let input = DiarizationInput {
                        transcript: segment.clone(),
                        speech_audio: vec![],
                        speech_start_time: Duration::from_secs_f64(start),
                        speech_end_time: Duration::from_secs_f64(end_time),
                    };
                    let diarized = diarization_worker.process_input(input);
                    diarization_count += 1;

                    let _ = ctx
                        .app_handle
                        .emit(events::SPEAKER_DETECTED, &diarized.speaker_info);
                    diarized.segment
                };

                log::debug!(
                    "Deepgram event receiver: emitted transcript #{} speaker={:?} \"{}\"",
                    asr_count,
                    final_segment.speaker_label,
                    &final_segment.text,
                );

                // SPEAKER_DETECTED was already emitted above (if needed) — pass
                // `None` here so the shared helper doesn't double-emit.
                emit_transcript_and_extract(
                    final_segment,
                    None,
                    &ctx,
                    asr_count,
                    diarization_count,
                    &extraction_count,
                    &graph_update_count,
                );
            }
            DeepgramEvent::Error { message } => {
                log::warn!("Deepgram event receiver: error: {message}");
                if let Ok(mut status) = ctx.pipeline_status.write() {
                    status.asr = StageStatus::Error {
                        message: format!("Deepgram error: {message}"),
                    };
                }
            }
            DeepgramEvent::Disconnected => {
                log::info!("Deepgram event receiver: disconnected");
                break;
            }
            DeepgramEvent::Connected => {
                log::debug!("Deepgram event receiver: connected event received");
            }
            DeepgramEvent::Reconnecting {
                attempt,
                backoff_secs,
            } => {
                // Auto-reconnect in flight — surface through pipeline status
                // so the UI can show a "reconnecting…" hint instead of
                // leaving the stage looking healthy.
                log::info!(
                    "Deepgram event receiver: reconnecting attempt={attempt} backoff={backoff_secs}s"
                );
                if let Ok(mut status) = ctx.pipeline_status.write() {
                    status.asr = StageStatus::Error {
                        message: format!(
                            "Deepgram reconnecting (attempt {attempt}, retry in {backoff_secs}s)"
                        ),
                    };
                }
            }
            DeepgramEvent::Reconnected => {
                log::info!("Deepgram event receiver: reconnected");
                if let Ok(mut status) = ctx.pipeline_status.write() {
                    // Preserve the running count across reconnects so the UI
                    // doesn't flash back to 0 transcripts.
                    status.asr = StageStatus::Running {
                        processed_count: asr_count,
                    };
                }
            }
        }
    }

    log::info!(
        "Deepgram event receiver: exiting. ASR segments={}, diarized={}",
        asr_count,
        diarization_count,
    );
}

// ---------------------------------------------------------------------------
// AssemblyAI streaming speech processor
// ---------------------------------------------------------------------------

/// AssemblyAI streaming speech processor — connects to the AssemblyAI real-time
/// WebSocket API, streams audio, and processes transcript events through the
/// same downstream pipeline (diarization, storage, events, extraction).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_assemblyai_speech_processor(
    processed_rx: Receiver<ProcessedAudioChunk>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    models_dir: PathBuf,
    llm_provider: LlmProvider,
    assemblyai_config: crate::asr::assemblyai::AssemblyAIConfig,
) {
    use crate::asr::assemblyai::AssemblyAIClient;

    // Create and connect the AssemblyAI client.
    let mut client = AssemblyAIClient::new(assemblyai_config);
    match client.connect() {
        Ok(()) => {
            log::info!("AssemblyAI streaming: connected successfully");
        }
        Err(e) => {
            log::error!("AssemblyAI streaming: failed to connect: {e}");
            if let Ok(mut status) = pipeline_status.write() {
                status.asr = StageStatus::Error {
                    message: format!("AssemblyAI connect failed: {e}"),
                };
            }
            return;
        }
    }

    let event_rx = client.event_rx();

    // Spawn the AssemblyAI event receiver thread (processes transcript results).
    let is_transcribing_rx = is_transcribing.clone();
    let _receiver_handle = std::thread::Builder::new()
        .name("assemblyai-event-rx".to_string())
        .spawn({
            let transcript_buffer = transcript_buffer.clone();
            let transcript_writer = transcript_writer.clone();
            let pipeline_status = pipeline_status.clone();
            let app_handle = app_handle.clone();
            let knowledge_graph = knowledge_graph.clone();
            let graph_snapshot = graph_snapshot.clone();
            let graph_extractor = graph_extractor.clone();
            let llm_engine = llm_engine.clone();
            let api_client = api_client.clone();
            let mistralrs_engine = mistralrs_engine.clone();
            let llm_provider = llm_provider.clone();
            let models_dir = models_dir.clone();

            move || {
                run_assemblyai_event_receiver(
                    event_rx,
                    is_transcribing_rx,
                    transcript_buffer,
                    transcript_writer,
                    pipeline_status,
                    app_handle,
                    knowledge_graph,
                    graph_snapshot,
                    graph_extractor,
                    llm_engine,
                    api_client,
                    mistralrs_engine,
                    models_dir,
                    llm_provider,
                );
            }
        });

    // Mark ASR as running.
    if let Ok(mut status) = pipeline_status.write() {
        status.asr = StageStatus::Running { processed_count: 0 };
    }

    // Audio sender loop: reads chunks and forwards to AssemblyAI.
    log::info!("AssemblyAI streaming: entering audio sender loop");
    let mut chunks_sent: u64 = 0;

    loop {
        let chunk = match processed_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(chunk) => chunk,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    log::info!(
                        "AssemblyAI streaming: is_transcribing flag cleared, exiting sender"
                    );
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                log::info!("AssemblyAI streaming: audio channel disconnected, exiting sender");
                break;
            }
        };

        if !is_transcribing.load(Ordering::Relaxed) {
            log::info!("AssemblyAI streaming: is_transcribing flag cleared, exiting sender");
            break;
        }

        // NOTE: intentionally no longer checks `client.is_connected()` — the
        // client's session task handles transient reconnects internally and
        // `send_audio` buffers during the reconnect window. A truly dead
        // client surfaces via `send_audio` returning "Audio channel closed".

        // Send audio directly to AssemblyAI (no accumulation needed).
        if let Err(e) = client.send_audio(&chunk.data) {
            log::warn!("AssemblyAI streaming: failed to send audio: {e}");
            break;
        }

        chunks_sent += 1;
        if chunks_sent % 100 == 0 {
            log::debug!("AssemblyAI streaming: sent {} audio chunks", chunks_sent);
        }
    }

    // Disconnect the client.
    client.disconnect();

    log::info!(
        "AssemblyAI streaming: audio sender exiting. Chunks sent={}",
        chunks_sent
    );
}

/// AssemblyAI event receiver thread — processes transcript events from the
/// AssemblyAI WebSocket and feeds them into the diarization + storage + events
/// + extraction pipeline (same downstream path as Deepgram).
#[allow(clippy::too_many_arguments)]
fn run_assemblyai_event_receiver(
    event_rx: crossbeam_channel::Receiver<crate::asr::assemblyai::AssemblyAIEvent>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    models_dir: PathBuf,
    llm_provider: LlmProvider,
) {
    use crate::asr::assemblyai::AssemblyAIEvent;
    use crate::diarization::{DiarizationInput, DiarizationWorker, DiarizedTranscript};

    let diarization_config = make_diarization_config(&models_dir);
    let (dummy_diar_tx, _dummy_diar_rx) = crossbeam_channel::unbounded::<DiarizedTranscript>();
    let mut diarization_worker = DiarizationWorker::new(diarization_config, dummy_diar_tx);

    let mut asr_count: u64 = 0;
    let mut diarization_count: u64 = 0;
    let extraction_count = Arc::new(AtomicU64::new(0));
    let graph_update_count = Arc::new(AtomicU64::new(0));

    // Track cumulative time offset for segments (AssemblyAI does not provide
    // absolute timestamps in the same way Deepgram does).
    let session_start = std::time::Instant::now();

    let ctx = TranscriptProcessingContext {
        transcript_buffer,
        transcript_writer,
        pipeline_status,
        app_handle,
        llm_engine,
        api_client,
        mistralrs_engine,
        llm_provider,
        graph_extractor,
        knowledge_graph,
        graph_snapshot,
    };

    log::info!("AssemblyAI event receiver: entering processing loop");

    loop {
        let event = match event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(ev) => ev,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    log::info!("AssemblyAI event receiver: is_transcribing flag cleared, exiting");
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                log::info!("AssemblyAI event receiver: event channel disconnected, exiting");
                break;
            }
        };

        if !is_transcribing.load(Ordering::Relaxed) {
            log::info!("AssemblyAI event receiver: is_transcribing flag cleared, exiting");
            break;
        }

        match event {
            AssemblyAIEvent::FinalTranscript { text, confidence } => {
                asr_count += 1;

                let now_secs = session_start.elapsed().as_secs_f64();
                // Approximate segment timing from session clock.
                let start_time = now_secs;
                let end_time = now_secs;

                let segment = TranscriptSegment {
                    id: uuid::Uuid::new_v4().to_string(),
                    source_id: "assemblyai-stream".to_string(),
                    speaker_id: None,
                    speaker_label: None,
                    text: text.clone(),
                    start_time,
                    end_time,
                    confidence: confidence as f32,
                };

                // Run through local diarization with empty audio (assigns
                // a default speaker when no audio signal is available).
                let input = DiarizationInput {
                    transcript: segment.clone(),
                    speech_audio: vec![],
                    speech_start_time: Duration::from_secs_f64(start_time),
                    speech_end_time: Duration::from_secs_f64(end_time),
                };
                let diarized = diarization_worker.process_input(input);
                diarization_count += 1;

                let _ = ctx
                    .app_handle
                    .emit(events::SPEAKER_DETECTED, &diarized.speaker_info);
                let final_segment = diarized.segment;

                log::debug!(
                    "AssemblyAI event receiver: emitted transcript #{} speaker={:?} \"{}\"",
                    asr_count,
                    final_segment.speaker_label,
                    &final_segment.text,
                );

                // SPEAKER_DETECTED was already emitted above — pass `None`
                // so the shared helper doesn't double-emit.
                emit_transcript_and_extract(
                    final_segment,
                    None,
                    &ctx,
                    asr_count,
                    diarization_count,
                    &extraction_count,
                    &graph_update_count,
                );
            }
            AssemblyAIEvent::PartialTranscript { text } => {
                log::debug!("AssemblyAI: interim transcript: \"{}\"", &text);
            }
            AssemblyAIEvent::Error { message } => {
                log::warn!("AssemblyAI event receiver: error: {message}");
                if let Ok(mut status) = ctx.pipeline_status.write() {
                    status.asr = StageStatus::Error {
                        message: format!("AssemblyAI error: {message}"),
                    };
                }
            }
            AssemblyAIEvent::SessionTerminated => {
                log::info!("AssemblyAI event receiver: session terminated");
                break;
            }
            AssemblyAIEvent::Reconnecting {
                attempt,
                backoff_secs,
            } => {
                log::info!(
                    "AssemblyAI event receiver: reconnecting attempt={attempt} backoff={backoff_secs}s"
                );
                if let Ok(mut status) = ctx.pipeline_status.write() {
                    status.asr = StageStatus::Error {
                        message: format!(
                            "AssemblyAI reconnecting (attempt {attempt}, retry in {backoff_secs}s)"
                        ),
                    };
                }
            }
            AssemblyAIEvent::Reconnected => {
                log::info!("AssemblyAI event receiver: reconnected");
                if let Ok(mut status) = ctx.pipeline_status.write() {
                    // Preserve the running count across reconnects so the UI
                    // doesn't flash back to 0 transcripts.
                    status.asr = StageStatus::Running {
                        processed_count: asr_count,
                    };
                }
            }
        }
    }

    log::info!(
        "AssemblyAI event receiver: exiting. ASR segments={}, diarized={}",
        asr_count,
        diarization_count,
    );
}

// ---------------------------------------------------------------------------
// AWS Transcribe streaming speech processor
// ---------------------------------------------------------------------------

pub(crate) fn run_aws_transcribe_speech_processor(
    processed_rx: Receiver<ProcessedAudioChunk>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    models_dir: PathBuf,
    llm_provider: LlmProvider,
    aws_config: crate::asr::aws_transcribe::AwsTranscribeConfig,
) {
    let diarization_config = make_diarization_config(&models_dir);
    let (dummy_diar_tx, _dummy_diar_rx) = crossbeam_channel::unbounded::<DiarizedTranscript>();
    let mut diarization_worker = DiarizationWorker::new(diarization_config, dummy_diar_tx);

    let mut asr_count: u64 = 0;
    let mut diarization_count: u64 = 0;
    let extraction_count = Arc::new(AtomicU64::new(0));
    let graph_update_count = Arc::new(AtomicU64::new(0));

    if let Ok(mut status) = pipeline_status.write() {
        status.asr = StageStatus::Running { processed_count: 0 };
    }

    log::info!("AWS Transcribe speech processor: starting streaming session");

    let pipeline_status_err = pipeline_status.clone();

    // Built from clones so the callback can move `ctx` while the outer
    // `pipeline_status_err` stays usable for error reporting after the
    // session returns.
    let ctx = TranscriptProcessingContext {
        transcript_buffer,
        transcript_writer,
        pipeline_status,
        app_handle,
        llm_engine,
        api_client,
        mistralrs_engine,
        llm_provider,
        graph_extractor,
        knowledge_graph,
        graph_snapshot,
    };

    let result = crate::asr::aws_transcribe::run_aws_transcribe_session(
        processed_rx,
        is_transcribing,
        aws_config,
        move |transcript| {
            asr_count += 1;

            let input = DiarizationInput {
                transcript,
                speech_audio: vec![],
                speech_start_time: Duration::ZERO,
                speech_end_time: Duration::ZERO,
            };
            let diarized = diarization_worker.process_input(input);
            diarization_count += 1;

            emit_transcript_and_extract(
                diarized.segment,
                Some(diarized.speaker_info),
                &ctx,
                asr_count,
                diarization_count,
                &extraction_count,
                &graph_update_count,
            );
        },
    );

    if let Err(e) = result {
        log::error!("AWS Transcribe session error: {}", e);
        if let Ok(mut status) = pipeline_status_err.write() {
            status.asr = StageStatus::Error { message: e };
        }
    }

    log::info!("AWS Transcribe speech processor: session ended");
}

// ---------------------------------------------------------------------------
// AccumulatedSegment → ASR bridge
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Sherpa-onnx streaming ASR speech processor
// ---------------------------------------------------------------------------

#[cfg(feature = "sherpa-streaming")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_sherpa_onnx_speech_processor(
    processed_rx: Receiver<ProcessedAudioChunk>,
    is_transcribing: Arc<AtomicBool>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    transcript_writer: Arc<Mutex<Option<crate::persistence::TranscriptWriter>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    mistralrs_engine: Arc<Mutex<Option<MistralRsEngine>>>,
    models_dir: PathBuf,
    llm_provider: LlmProvider,
    sherpa_config: crate::asr::sherpa_streaming::SherpaStreamingConfig,
) {
    use crate::asr::sherpa_streaming::SherpaStreamingWorker;
    use crate::diarization::{DiarizationInput, DiarizationWorker, DiarizedTranscript};

    let mut worker = match SherpaStreamingWorker::new(&sherpa_config) {
        Ok(w) => w,
        Err(e) => {
            log::error!("Sherpa-onnx streaming: failed to create worker: {e}");
            if let Ok(mut status) = pipeline_status.write() {
                status.asr = StageStatus::Error {
                    message: format!("Sherpa-onnx init failed: {e}"),
                };
            }
            run_speech_processor_diarization_only(
                processed_rx,
                is_transcribing,
                transcript_buffer,
                transcript_writer,
                pipeline_status,
                app_handle,
                knowledge_graph,
                graph_snapshot,
                graph_extractor,
                llm_engine,
                api_client,
                mistralrs_engine,
                models_dir,
                llm_provider,
            );
            return;
        }
    };

    let diarization_config = make_diarization_config(&models_dir);
    let (dummy_diar_tx, _dummy_diar_rx) = crossbeam_channel::unbounded::<DiarizedTranscript>();
    let mut diarization_worker = DiarizationWorker::new(diarization_config, dummy_diar_tx);

    let mut asr_count: u64 = 0;
    let mut diarization_count: u64 = 0;
    let extraction_count = Arc::new(AtomicU64::new(0));
    let graph_update_count = Arc::new(AtomicU64::new(0));
    let session_start = std::time::Instant::now();
    let mut utterance_start = std::time::Instant::now();

    if let Ok(mut status) = pipeline_status.write() {
        status.asr = StageStatus::Running { processed_count: 0 };
    }

    let ctx = TranscriptProcessingContext {
        transcript_buffer,
        transcript_writer,
        pipeline_status,
        app_handle,
        llm_engine,
        api_client,
        mistralrs_engine,
        llm_provider,
        graph_extractor,
        knowledge_graph,
        graph_snapshot,
    };

    log::info!("Sherpa-onnx streaming: entering processing loop");
    let mut chunks_processed: u64 = 0;

    loop {
        let chunk = match processed_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(chunk) => chunk,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !is_transcribing.load(Ordering::Relaxed) {
                    log::info!("Sherpa-onnx streaming: is_transcribing flag cleared, exiting");
                    break;
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                log::info!("Sherpa-onnx streaming: audio channel disconnected, exiting");
                break;
            }
        };

        if !is_transcribing.load(Ordering::Relaxed) {
            log::info!("Sherpa-onnx streaming: is_transcribing flag cleared, exiting");
            break;
        }

        chunks_processed += 1;

        if let Some((text, is_endpoint)) = worker.process_chunk(&chunk.data) {
            if is_endpoint {
                asr_count += 1;
                let end_time = session_start.elapsed().as_secs_f64();
                let start_time = end_time - utterance_start.elapsed().as_secs_f64();
                utterance_start = std::time::Instant::now();

                let segment = TranscriptSegment {
                    id: uuid::Uuid::new_v4().to_string(),
                    source_id: chunk.source_id.clone(),
                    speaker_id: None,
                    speaker_label: None,
                    text: text.clone(),
                    start_time,
                    end_time,
                    confidence: 0.9,
                };

                let input = DiarizationInput {
                    transcript: segment,
                    speech_audio: vec![],
                    speech_start_time: Duration::from_secs_f64(start_time),
                    speech_end_time: Duration::from_secs_f64(end_time),
                };
                let diarized = diarization_worker.process_input(input);
                diarization_count += 1;

                let _ = ctx
                    .app_handle
                    .emit(events::SPEAKER_DETECTED, &diarized.speaker_info);
                let final_segment = diarized.segment;

                log::debug!(
                    "Sherpa-onnx streaming: emitted transcript #{} speaker={:?} \"{}\"",
                    asr_count,
                    final_segment.speaker_label,
                    &final_segment.text,
                );

                // SPEAKER_DETECTED was already emitted above — pass `None`
                // so the shared helper doesn't double-emit.
                emit_transcript_and_extract(
                    final_segment,
                    None,
                    &ctx,
                    asr_count,
                    diarization_count,
                    &extraction_count,
                    &graph_update_count,
                );
            }
        }

        if chunks_processed % 500 == 0 {
            log::debug!(
                "Sherpa-onnx streaming: processed {} chunks, {} transcripts",
                chunks_processed,
                asr_count
            );
        }
    }

    log::info!(
        "Sherpa-onnx streaming: exiting. Chunks={}, ASR={}, diarized={}",
        chunks_processed,
        asr_count,
        diarization_count,
    );
}

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
