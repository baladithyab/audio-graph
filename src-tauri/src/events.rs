//! Tauri event name constants and payload types.
//!
//! These constants define the event names emitted from the Rust backend
//! to the frontend. The frontend subscribes using `listen()` from `@tauri-apps/api`.

/// Event emitted when a new transcript segment is available.
pub const TRANSCRIPT_UPDATE: &str = "transcript-update";

/// Event emitted when the knowledge graph changes (full snapshot).
/// Emitted less frequently (every 10th update or every 30 seconds).
pub const GRAPH_UPDATE: &str = "graph-update";

/// Event emitted with incremental graph changes (delta updates).
/// Emitted on every extraction cycle for efficient frontend updates.
pub const GRAPH_DELTA: &str = "graph-delta";

/// Event emitted periodically (every ~2s) or on status change.
pub const PIPELINE_STATUS_EVENT: &str = "pipeline-status";

/// Event emitted when a new speaker is first identified.
pub const SPEAKER_DETECTED: &str = "speaker-detected";

/// Event emitted when a capture error occurs.
pub const CAPTURE_ERROR: &str = "capture-error";

/// Event emitted when Gemini Live produces a transcription.
pub const GEMINI_TRANSCRIPTION: &str = "gemini-transcription";

/// Event emitted when Gemini Live produces a model response.
pub const GEMINI_RESPONSE: &str = "gemini-response";

/// Event emitted when the Gemini Live connection status changes.
pub const GEMINI_STATUS: &str = "gemini-status";

/// Status of an individual pipeline stage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum StageStatus {
    Idle,
    Running { processed_count: u64 },
    Error { message: String },
}

impl Default for StageStatus {
    fn default() -> Self {
        StageStatus::Idle
    }
}

/// Overall pipeline status, combining all stages.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PipelineStatus {
    pub capture: StageStatus,
    pub pipeline: StageStatus,
    pub asr: StageStatus,
    pub diarization: StageStatus,
    pub entity_extraction: StageStatus,
    pub graph: StageStatus,
}

/// Payload for capture error events.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureErrorPayload {
    pub source_id: String,
    pub error: String,
    pub recoverable: bool,
}

/// Emit a Tauri event and log any emission failure at `error` level.
///
/// The default `let _ = app.emit(...)` pattern silently swallows emission
/// errors, which makes failed frontend notifications undebuggable. Use this
/// helper instead so failures surface in logs.
pub fn emit_or_log<P>(app: &tauri::AppHandle, event: &str, payload: P)
where
    P: serde::Serialize + Clone,
{
    use tauri::Emitter;
    if let Err(e) = app.emit(event, payload) {
        log::error!("Failed to emit event '{}': {}", event, e);
    }
}

/// Heuristic classifier for capture errors into recoverable vs fatal.
///
/// Used at capture-error emit sites to populate `CaptureErrorPayload.recoverable`.
/// Fatal errors indicate the source cannot be used again without user action
/// (permission, device disconnection). Recoverable errors may succeed on retry.
pub fn classify_capture_error(err: &str) -> bool {
    let lower = err.to_lowercase();
    let fatal_markers = [
        "permission denied",
        "not permitted",
        "unauthorized",
        "disconnected",
        "device not found",
        "no such device",
        "device removed",
        "access denied",
        "not supported",
        "unsupported",
    ];
    if fatal_markers.iter().any(|m| lower.contains(m)) {
        return false;
    }
    // Default to recoverable for unclassified errors — user can retry.
    true
}
