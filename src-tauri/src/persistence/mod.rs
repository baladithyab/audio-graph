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
use std::sync::OnceLock;

use crate::state::TranscriptSegment;

pub mod io;
pub use io::write_or_emit_storage_full;

/// User-facing retry after a `capture-storage-full` banner dismissal.
///
/// Probes the transcripts directory with a tiny write. On success, clears the
/// process-wide storage-full flag so the next real ENOSPC will re-emit, and
/// returns `Ok(())` — the banner should dismiss. On failure (disk still
/// full), leaves the flag set and returns `Err(io::Error)` so the UI keeps
/// the banner visible and can show the user they still need to free space.
///
/// Probing writes rather than trusting the writer-thread state: the writer
/// may not have attempted another segment since the failure, so only a real
/// write can confirm the disk is healthy again.
pub fn retry_storage_write() -> Result<(), std::io::Error> {
    let dir = transcripts_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Transcripts directory could not be resolved (no HOME?)",
        )
    })?;
    io::probe_writable(&dir)?;
    io::clear_storage_full_flag();
    Ok(())
}

// ---------------------------------------------------------------------------
// AppHandle registration for background persistence threads
// ---------------------------------------------------------------------------
//
// The transcript writer and graph autosave threads are spawned before the
// Tauri runtime's `AppHandle` is threaded into the app state (and the
// spawn-site in `lib.rs` is intentionally untouched by this module). They
// still need an `AppHandle` to emit `CAPTURE_STORAGE_FULL` events on
// disk-full errors, so we stash one in a process-wide `OnceLock` that the
// speech processor (which receives an `AppHandle` at startup) initialises.
//
// If the handle hasn't been registered yet — e.g. a disk-full error fires
// before any speech processor has started — we fall back to logging only.

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Register the Tauri `AppHandle` that persistence background threads should
/// use when emitting `CAPTURE_STORAGE_FULL` events. Safe to call repeatedly;
/// only the first call wins.
pub fn register_app_handle(handle: tauri::AppHandle) {
    let _ = APP_HANDLE.set(handle);
}

/// Return the registered `AppHandle`, if one has been set.
pub(crate) fn app_handle() -> Option<&'static tauri::AppHandle> {
    APP_HANDLE.get()
}

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
    /// Writer thread handle. Taken by `shutdown_with_timeout` so the caller
    /// can wait on it with a bounded timeout; left as `None` after that.
    /// On drop-without-shutdown the handle is simply released (detached).
    handle: Option<std::thread::JoinHandle<()>>,
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
                        // Classify the open error too — a user out of disk
                        // can hit ENOSPC on the very first file creation.
                        io::handle_write_error(app_handle(), &file_path, 0, 0, &e);
                        return;
                    }
                };
                // Lock down perms as soon as the file exists. Transcripts can
                // contain sensitive speech content.
                crate::fs_util::set_owner_only(&file_path);
                let mut writer = BufWriter::new(file);

                // `io::handle_write_error` owns the "first ENOSPC emits, rest
                // log" debounce via the process-wide `STORAGE_FULL_ACTIVE`
                // atomic, so this loop can forward every error through it
                // without its own local flag. The retry command resets the
                // atomic after a successful probe, which in turn lets the
                // *next* real ENOSPC re-emit.
                while let Ok(msg) = rx.recv() {
                    match msg {
                        TranscriptWriteMsg::Append(segment) => {
                            match serde_json::to_string(&segment) {
                                Ok(json) => {
                                    // `writeln!` includes the trailing '\n',
                                    // so `bytes_lost` is json.len() + 1.
                                    let bytes_lost = json.len() as u64 + 1;
                                    if let Err(e) = writeln!(writer, "{}", json) {
                                        io::handle_write_error(
                                            app_handle(),
                                            &file_path,
                                            0,
                                            bytes_lost,
                                            &e,
                                        );
                                    }
                                }
                                Err(e) => {
                                    log::warn!("Transcript writer: serialize error: {}", e);
                                }
                            }
                        }
                        TranscriptWriteMsg::Shutdown => {
                            if let Err(e) = writer.flush() {
                                io::handle_write_error(app_handle(), &file_path, 0, 0, &e);
                            }
                            break;
                        }
                    }
                }

                // Final flush on channel close
                if let Err(e) = writer.flush() {
                    io::handle_write_error(app_handle(), &file_path, 0, 0, &e);
                }
                log::info!("Transcript writer: shut down for {:?}", file_path);
            })
            .ok()?;

        Some(Self {
            tx,
            handle: Some(handle),
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
    ///
    /// Non-blocking: sends the `Shutdown` message and returns. The thread
    /// will exit on its own after draining the channel. Use
    /// [`Self::shutdown_with_timeout`] when the caller needs bounded assurance
    /// that flush completed before moving on.
    pub fn shutdown(&self) {
        let _ = self.tx.send(TranscriptWriteMsg::Shutdown);
    }

    /// Signal shutdown and wait (up to `timeout`) for the writer thread to exit.
    ///
    /// Returns `true` if the thread joined within the timeout, `false` if the
    /// wait expired (the thread is left detached — it will eventually exit on
    /// its own when the underlying I/O unsticks, or be torn down at process
    /// exit). On `false` the caller should assume some un-flushed segments may
    /// still be in the writer's BufWriter and proceed with spawning a new
    /// writer anyway — the alternative is blocking the rotation IPC
    /// indefinitely on a wedged disk, which is worse than a rare lost tail.
    ///
    /// Implementation note: `JoinHandle::join` is blocking with no timeout
    /// overload in std. We move the handle into a watchdog thread that
    /// performs the join, and signal completion via a `mpsc` channel so the
    /// calling thread can `recv_timeout`. On timeout the watchdog is itself
    /// leaked — the JoinHandle inside it prevents the writer thread from
    /// becoming a true zombie, just an unobserved one.
    pub fn shutdown_with_timeout(mut self, timeout: std::time::Duration) -> bool {
        let _ = self.tx.send(TranscriptWriteMsg::Shutdown);
        let Some(handle) = self.handle.take() else {
            return true;
        };
        let (done_tx, done_rx) = mpsc::channel::<()>();
        // Watchdog thread: blocks on join, then signals. If join panics in the
        // writer thread we still signal (the `Err` from join is just a panic
        // propagation; we're shutting down anyway).
        let spawned = std::thread::Builder::new()
            .name("transcript-writer-join".to_string())
            .spawn(move || {
                let _ = handle.join();
                let _ = done_tx.send(());
            });
        match spawned {
            Ok(_watchdog) => {
                // `_watchdog`'s JoinHandle is dropped here (detached). That's
                // fine: its lifetime is bounded by the writer thread's join,
                // which is what we want. We wait on done_rx only.
                done_rx.recv_timeout(timeout).is_ok()
            }
            Err(e) => {
                log::warn!(
                    "Failed to spawn transcript-writer-join watchdog: {} — \
                     writer thread is detached",
                    e
                );
                // Couldn't spawn the watchdog; we can't bound the wait, so
                // report "timed out" rather than block the caller.
                false
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Knowledge graph persistence
// ---------------------------------------------------------------------------

/// Save a serializable value as pretty-printed JSON to a file.
///
/// Uses an atomic write (tmp file + rename) so a partial write never replaces
/// a known-good file. I/O errors are classified via [`io::handle_write_error`]
/// so ENOSPC on the tmp file emits `CAPTURE_STORAGE_FULL` to the UI; other
/// errors fall through to the legacy string-return path.
pub fn save_json<T: serde::Serialize>(value: &T, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }

    // Atomic write: write to temp file, then rename
    let tmp_path = path.with_extension("json.tmp");
    let file = match fs::File::create(&tmp_path) {
        Ok(f) => f,
        Err(e) => {
            io::handle_write_error(app_handle(), &tmp_path, 0, 0, &e);
            return Err(format!("Failed to create temp file {:?}: {}", tmp_path, e));
        }
    };
    let mut writer = BufWriter::new(file);
    if let Err(e) = serde_json::to_writer_pretty(&mut writer, value) {
        // `serde_json::Error::classify() == Category::Io` indicates the
        // underlying writer failed — surface storage-full conditions via
        // the shared handler before returning.
        if e.classify() == serde_json::error::Category::Io {
            let io_err = std::io::Error::from(e);
            io::handle_write_error(app_handle(), &tmp_path, 0, 0, &io_err);
            return Err(format!("Failed to serialize to {:?}: {}", tmp_path, io_err));
        }
        return Err(format!("Failed to serialize to {:?}: {}", tmp_path, e));
    }
    if let Err(e) = writer.flush() {
        io::handle_write_error(app_handle(), &tmp_path, 0, 0, &e);
        return Err(format!("Failed to flush {:?}: {}", tmp_path, e));
    }
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
/// `session_id` is shared via `Arc<RwLock<String>>` so
/// [`AppState::rotate_session`](crate::state::AppState::rotate_session) can
/// repoint the autosave target mid-run without respawning this thread. Each
/// tick snapshots the current ID once at entry and uses that single value for
/// both the file path *and* the `update_stats` call — so even if a rotation
/// lands mid-tick, the tick's writes all target the same session.
///
/// `rotation_in_progress` is the shared guard from `AppState`: if a rotation
/// is actively swapping the writer/session_id when the tick fires, we skip
/// this tick and wait for the next one rather than race the rotation.
///
/// Returns the thread handle (or `None` if the graphs directory cannot be resolved).
pub fn spawn_graph_autosave(
    session_id: Arc<RwLock<String>>,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    rotation_in_progress: Arc<std::sync::atomic::AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let dir = graphs_dir()?;
    if let Err(e) = ensure_dir(&dir) {
        log::warn!("Graph auto-save disabled: {}", e);
        return None;
    }

    let handle = std::thread::Builder::new()
        .name("graph-autosave".to_string())
        .spawn(move || {
            log::info!("Graph auto-save: started (every 30s → {:?})", dir);
            loop {
                std::thread::sleep(std::time::Duration::from_secs(30));

                // If a rotation is mid-flight, skip this tick. The in-flight
                // rotation will land soon; the next tick (at most 30s later)
                // will observe the new session ID atomically. Avoids the
                // window where we could write graph state to the old session
                // file concurrently with the writer-respawn for the new one.
                if rotation_in_progress.load(std::sync::atomic::Ordering::SeqCst) {
                    log::debug!("Graph auto-save: skipping tick (rotation in progress)");
                    continue;
                }

                // Snapshot session_id ONCE at tick entry. Every subsequent
                // write in this tick uses `current_sid` — never re-reads
                // `session_id` — so the file path and the stats update are
                // guaranteed to target the same session even if a rotation
                // lands between sub-steps. Poisoned lock → recover; the
                // inner String has no broken invariant.
                let current_sid = match session_id.read() {
                    Ok(g) => g.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let file_path = dir.join(format!("{}.json", current_sid));

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
                // Pass the tick-start-cached `current_sid`, NOT a fresh read
                // of session_id, so the stats update matches the file we
                // just wrote above.
                if let Err(e) = crate::sessions::update_stats(
                    &current_sid,
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
