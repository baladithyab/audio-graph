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

/// Event emitted when a persistence write fails because the underlying storage
/// is full (ENOSPC / ERROR_DISK_FULL). The frontend should surface this as a
/// user-visible error so the operator can free disk space before more
/// transcript/graph data is lost.
pub const CAPTURE_STORAGE_FULL: &str = "capture-storage-full";

/// Event emitted when the backpressure state of a capture source changes —
/// i.e. the rsac ring buffer has started or stopped dropping buffers because
/// the consumer (this app's pipeline) isn't keeping up. Edge-triggered: fires
/// only on transitions (false→true or true→false), not continuously.
pub const CAPTURE_BACKPRESSURE: &str = "capture-backpressure";

/// Event emitted when Gemini Live produces a transcription.
pub const GEMINI_TRANSCRIPTION: &str = "gemini-transcription";

/// Event emitted when Gemini Live produces a model response.
pub const GEMINI_RESPONSE: &str = "gemini-response";

/// Event emitted when the Gemini Live connection status changes.
pub const GEMINI_STATUS: &str = "gemini-status";

/// Event emitted throughout a model download with elapsed + byte counters so
/// the frontend can compute an ETA. Throttled to roughly 1 Hz; also fires once
/// on completion or error.
pub const MODEL_DOWNLOAD_PROGRESS: &str = "model-download-progress";

/// Event emitted when an AWS call (Transcribe streaming, STS preflight) fails
/// with a credential- or region-class error that the frontend should surface
/// via a localized toast with recovery guidance (ag#13).
pub const AWS_ERROR: &str = "aws-error";

/// Status of an individual pipeline stage.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum StageStatus {
    #[default]
    Idle,
    Running {
        processed_count: u64,
    },
    Error {
        message: String,
    },
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

/// Payload for `CAPTURE_STORAGE_FULL` events.
///
/// Emitted when a persistence write fails because the underlying storage is
/// full (ENOSPC / ERROR_DISK_FULL). Use the `bytes_lost` field to tell the
/// user how much data failed to hit disk on this attempt; `bytes_written`
/// is best-effort and is `0` when the error happens on the initial open.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureStorageFullPayload {
    /// Absolute path the app tried to write to.
    pub path: String,
    /// Bytes successfully written before the error (best-effort).
    pub bytes_written: u64,
    /// Bytes the app was trying to write when the error occurred (best-effort:
    /// the size of the buffer we were attempting to persist).
    pub bytes_lost: u64,
}

/// Payload for capture-backpressure state-change events.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureBackpressurePayload {
    pub source_id: String,
    /// `true` when the ring buffer has started dropping; `false` when recovery
    /// is detected. The frontend should surface this as a transient warning
    /// (e.g. a pill badge) rather than a fatal error.
    pub is_backpressured: bool,
}

/// Payload for `AWS_ERROR` events (ag#13).
///
/// `error` carries the structured classification (a [`crate::aws_util::UiAwsError`]
/// serialized with `category` / payload fields). `raw_message` is the original aws-sdk
/// error string, kept so the frontend can log or disclose details when the
/// category alone isn't enough (e.g. unexpected `Unknown` bucket).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AwsErrorPayload {
    pub error: crate::aws_util::UiAwsError,
    pub raw_message: String,
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

/// Returns `true` if this I/O error indicates the underlying storage is full
/// (ENOSPC on Unix, ERROR_DISK_FULL on Windows).
///
/// `std::io::ErrorKind::StorageFull` was stabilised relatively recently and
/// the mapping from raw OS codes into that kind still varies across Rust
/// versions and platforms, so we check both the kind and the `raw_os_error`
/// signatures defensively — whichever trips first wins.
pub fn is_storage_full(err: &std::io::Error) -> bool {
    // Prefer the symbolic kind when available; fall through to raw_os_error
    // if the current toolchain doesn't map the error to `StorageFull` yet.
    if err.kind() == std::io::ErrorKind::StorageFull {
        return true;
    }
    matches!(err.raw_os_error(), Some(28) | Some(112))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_storage_full_detects_enospc() {
        // ENOSPC is 28 on Linux and macOS.
        let err = std::io::Error::from_raw_os_error(28);
        assert!(is_storage_full(&err));
    }

    #[test]
    fn is_storage_full_detects_windows_disk_full() {
        // ERROR_DISK_FULL is 112 on Windows.
        let err = std::io::Error::from_raw_os_error(112);
        assert!(is_storage_full(&err));
    }

    #[test]
    fn is_storage_full_ignores_unrelated_errors() {
        // EACCES / generic errors must not be misclassified as storage-full.
        let err = std::io::Error::from_raw_os_error(13);
        assert!(!is_storage_full(&err));

        let other = std::io::Error::other("boom");
        assert!(!is_storage_full(&other));
    }
}
