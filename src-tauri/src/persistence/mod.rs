//! File-based persistence for transcripts and knowledge graph snapshots.
//!
//! Transcripts are appended as JSON lines (`.jsonl`) to a session file.
//! The knowledge graph is serialized as a single JSON file.
//!
//! All file I/O is performed asynchronously via a dedicated writer thread
//! to avoid blocking the speech processor or UI thread.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::state::TranscriptSegment;

// ---------------------------------------------------------------------------
// Base directory resolution
// ---------------------------------------------------------------------------

/// Resolve the base data directory (`~/.audiograph/`).
///
/// Uses `$HOME` on Unix and `%USERPROFILE%` on Windows.
fn base_data_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    let home = std::env::var("HOME").ok();
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").ok();
    #[cfg(not(any(unix, windows)))]
    let home: Option<String> = None;

    home.map(|h| PathBuf::from(h).join(".audiograph"))
}

/// Resolve the transcripts directory (`~/.audiograph/transcripts/`).
pub fn transcripts_dir() -> Option<PathBuf> {
    base_data_dir().map(|d| d.join("transcripts"))
}

/// Resolve the graphs directory (`~/.audiograph/graphs/`).
pub fn graphs_dir() -> Option<PathBuf> {
    base_data_dir().map(|d| d.join("graphs"))
}

/// Ensure a directory exists, creating it (and parents) if necessary.
fn ensure_dir(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create directory {:?}: {}", dir, e))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Async transcript writer (channel-based)
// ---------------------------------------------------------------------------

/// Messages sent to the transcript writer thread.
pub enum TranscriptWriteMsg {
    /// Append a transcript segment as a JSON line.
    Append(TranscriptSegment),
    /// Flush the writer and shut down.
    Shutdown,
}

/// Handle to the transcript writer thread.
pub struct TranscriptWriter {
    tx: mpsc::Sender<TranscriptWriteMsg>,
    _handle: std::thread::JoinHandle<()>,
}

impl TranscriptWriter {
    /// Spawn a new transcript writer thread for the given session.
    ///
    /// Returns `None` if the base directory cannot be resolved or created.
    pub fn spawn(session_id: &str) -> Option<Self> {
        let dir = transcripts_dir()?;
        if let Err(e) = ensure_dir(&dir) {
            log::warn!("Transcript persistence disabled: {}", e);
            return None;
        }

        let file_path = dir.join(format!("{}.jsonl", session_id));
        let (tx, rx) = mpsc::channel::<TranscriptWriteMsg>();

        let handle = std::thread::Builder::new()
            .name("transcript-writer".to_string())
            .spawn(move || {
                let file = match fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&file_path)
                {
                    Ok(f) => f,
                    Err(e) => {
                        log::warn!("Transcript writer: failed to open {:?}: {}", file_path, e);
                        return;
                    }
                };
                // Lock down perms as soon as the file exists. Transcripts can
                // contain sensitive speech content.
                crate::fs_util::set_owner_only(&file_path);
                let mut writer = BufWriter::new(file);

                while let Ok(msg) = rx.recv() {
                    match msg {
                        TranscriptWriteMsg::Append(segment) => {
                            match serde_json::to_string(&segment) {
                                Ok(json) => {
                                    if let Err(e) = writeln!(writer, "{}", json) {
                                        log::warn!("Transcript writer: write error: {}", e);
                                    }
                                }
                                Err(e) => {
                                    log::warn!("Transcript writer: serialize error: {}", e);
                                }
                            }
                        }
                        TranscriptWriteMsg::Shutdown => {
                            let _ = writer.flush();
                            break;
                        }
                    }
                }

                // Final flush on channel close
                let _ = writer.flush();
                log::info!("Transcript writer: shut down for {:?}", file_path);
            })
            .ok()?;

        Some(Self {
            tx,
            _handle: handle,
        })
    }

    /// Enqueue a transcript segment for writing. Non-blocking.
    pub fn append(&self, segment: &TranscriptSegment) {
        // Best-effort; if the channel is full or closed, we log and move on.
        if let Err(e) = self.tx.send(TranscriptWriteMsg::Append(segment.clone())) {
            log::warn!("Transcript writer: failed to enqueue segment: {}", e);
        }
    }

    /// Signal the writer to flush and shut down.
    pub fn shutdown(&self) {
        let _ = self.tx.send(TranscriptWriteMsg::Shutdown);
    }
}

// ---------------------------------------------------------------------------
// Knowledge graph persistence
// ---------------------------------------------------------------------------

/// Save a serializable value as pretty-printed JSON to a file.
pub fn save_json<T: serde::Serialize>(value: &T, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }

    // Atomic write: write to temp file, then rename
    let tmp_path = path.with_extension("json.tmp");
    let file = fs::File::create(&tmp_path)
        .map_err(|e| format!("Failed to create temp file {:?}: {}", tmp_path, e))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)
        .map_err(|e| format!("Failed to serialize to {:?}: {}", tmp_path, e))?;
    writer
        .flush()
        .map_err(|e| format!("Failed to flush {:?}: {}", tmp_path, e))?;
    drop(writer);

    // Lock down perms on the tmp file before rename. Graph JSON can contain
    // excerpts of transcribed speech that should not be world-readable.
    crate::fs_util::set_owner_only(&tmp_path);

    fs::rename(&tmp_path, path)
        .map_err(|e| format!("Failed to rename {:?} → {:?}: {}", tmp_path, path, e))?;

    // Re-apply after rename in case rename semantics differ across platforms.
    crate::fs_util::set_owner_only(path);

    Ok(())
}

/// Load a deserializable value from a JSON file.
pub fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let data = fs::read_to_string(path).map_err(|e| format!("Failed to read {:?}: {}", path, e))?;
    serde_json::from_str(&data).map_err(|e| format!("Failed to deserialize {:?}: {}", path, e))
}

// ---------------------------------------------------------------------------
// Graph auto-save timer
// ---------------------------------------------------------------------------

use crate::graph::temporal::TemporalKnowledgeGraph;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

/// Spawn a background thread that auto-saves the knowledge graph every 30 seconds
/// and refreshes the session index stats (segment/speaker/entity counts).
///
/// Returns the thread handle (or `None` if the graphs directory cannot be resolved).
pub fn spawn_graph_autosave(
    session_id: &str,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
) -> Option<std::thread::JoinHandle<()>> {
    let dir = graphs_dir()?;
    if let Err(e) = ensure_dir(&dir) {
        log::warn!("Graph auto-save disabled: {}", e);
        return None;
    }

    let file_path = dir.join(format!("{}.json", session_id));
    let session_id_owned = session_id.to_string();

    let handle = std::thread::Builder::new()
        .name("graph-autosave".to_string())
        .spawn(move || {
            log::info!("Graph auto-save: started (every 30s → {:?})", file_path);
            loop {
                std::thread::sleep(std::time::Duration::from_secs(30));

                // ── Graph snapshot: save to disk + capture entity count ────────
                let entity_count: u64 = {
                    let graph = match knowledge_graph.lock() {
                        Ok(g) => g,
                        Err(e) => {
                            log::warn!("Graph auto-save: lock poisoned, recovering: {}", e);
                            e.into_inner()
                        }
                    };

                    let node_count = graph.node_count();
                    if node_count > 0 {
                        if let Err(e) = graph.save_to_file(&file_path) {
                            log::warn!("Graph auto-save: failed: {}", e);
                        }
                    }
                    node_count as u64
                };

                // ── Transcript buffer: segment + unique speaker counts ─────────
                let (segment_count, speaker_count): (u64, u64) = match transcript_buffer.read() {
                    Ok(buf) => {
                        let segments = buf.len() as u64;
                        let speakers: HashSet<&str> =
                            buf.iter().filter_map(|s| s.speaker_id.as_deref()).collect();
                        (segments, speakers.len() as u64)
                    }
                    Err(e) => {
                        log::warn!("Graph auto-save: transcript buffer lock poisoned: {}", e);
                        let buf = e.into_inner();
                        let segments = buf.len() as u64;
                        let speakers: HashSet<&str> =
                            buf.iter().filter_map(|s| s.speaker_id.as_deref()).collect();
                        (segments, speakers.len() as u64)
                    }
                };

                // ── Refresh session index stats ────────────────────────────────
                if let Err(e) = crate::sessions::update_stats(
                    &session_id_owned,
                    segment_count,
                    speaker_count,
                    entity_count,
                ) {
                    log::warn!("Graph auto-save: session stats update failed: {}", e);
                }
            }
        })
        .ok()?;

    Some(handle)
}
