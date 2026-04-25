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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};

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

/// Poll interval for the writer's `recv_timeout`. Small enough that shutdown
/// latency is ~tens of ms on an idle channel, large enough that we don't burn
/// CPU when no segments are arriving.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

fn write_segment(writer: &mut BufWriter<fs::File>, segment: &TranscriptSegment, file_path: &Path) {
    match serde_json::to_string(segment) {
        Ok(json) => {
            let bytes_lost = json.len() as u64 + 1;
            if let Err(e) = writeln!(writer, "{}", json) {
                io::handle_write_error(app_handle(), file_path, 0, bytes_lost, &e);
            }
        }
        Err(e) => {
            log::warn!("Transcript writer: serialize error: {}", e);
        }
    }
}

/// Drain any buffered messages after shutdown is requested, so segments that
/// were already in the channel when `shutdown_requested` flipped still land on
/// disk. Stops at the first `Shutdown` message or when the channel empties.
fn drain_remaining(
    rx: &mpsc::Receiver<TranscriptWriteMsg>,
    writer: &mut BufWriter<fs::File>,
    file_path: &Path,
) {
    while let Ok(msg) = rx.try_recv() {
        match msg {
            TranscriptWriteMsg::Append(segment) => {
                write_segment(writer, &segment, file_path);
            }
            TranscriptWriteMsg::Shutdown => break,
        }
    }
}

/// Handle to the transcript writer thread.
pub struct TranscriptWriter {
    tx: mpsc::Sender<TranscriptWriteMsg>,
    /// Writer thread handle. Taken by `shutdown_with_timeout` so the caller
    /// can wait on it with a bounded timeout; left as `None` after that.
    /// On drop-without-shutdown the handle is simply released (detached).
    handle: Option<std::thread::JoinHandle<()>>,
    /// Shutdown flag shared with the writer thread. Set by `shutdown()` /
    /// `shutdown_with_timeout()`; the writer's `recv_timeout` poll checks it
    /// each tick and exits promptly even if no `Shutdown` message is drained.
    /// Dropping the `Sender` alone is not enough — if the channel still has
    /// buffered `Append` messages, the writer would keep flushing them before
    /// seeing the hang-up, holding the file handle open. The flag lets the
    /// writer short-circuit after draining what's already queued, so a new
    /// writer on the same file path can't overlap.
    shutdown_requested: Arc<AtomicBool>,
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
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let shutdown_flag = shutdown_requested.clone();
        let thread_path = file_path.clone();

        let handle = std::thread::Builder::new()
            .name("transcript-writer".to_string())
            .spawn(move || {
                let file = match fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&thread_path)
                {
                    Ok(f) => f,
                    Err(e) => {
                        // Classify the open error too — a user out of disk
                        // can hit ENOSPC on the very first file creation.
                        io::handle_write_error(app_handle(), &thread_path, 0, 0, &e);
                        return;
                    }
                };
                // Lock down perms as soon as the file exists. Transcripts can
                // contain sensitive speech content.
                crate::fs_util::set_owner_only(&thread_path);
                let mut writer = BufWriter::new(file);

                // `io::handle_write_error` owns the "first ENOSPC emits, rest
                // log" debounce via the process-wide `STORAGE_FULL_ACTIVE`
                // atomic, so this loop can forward every error through it
                // without its own local flag. The retry command resets the
                // atomic after a successful probe, which in turn lets the
                // *next* real ENOSPC re-emit.
                //
                // Use `recv_timeout` instead of `recv` so we can poll the
                // shutdown flag each tick. Without this, a slow drain of
                // buffered `Append` messages would delay the writer's exit,
                // keeping the file handle open past the point where a new
                // writer (for a rotated session) wants to open the same path.
                'outer: loop {
                    match rx.recv_timeout(POLL_INTERVAL) {
                        Ok(TranscriptWriteMsg::Append(segment)) => {
                            write_segment(&mut writer, &segment, &thread_path);
                            // After writing, if shutdown was requested, drain
                            // anything already queued (best-effort) and exit.
                            if shutdown_flag.load(Ordering::SeqCst) {
                                drain_remaining(&rx, &mut writer, &thread_path);
                                break 'outer;
                            }
                        }
                        Ok(TranscriptWriteMsg::Shutdown) => {
                            break 'outer;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if shutdown_flag.load(Ordering::SeqCst) {
                                drain_remaining(&rx, &mut writer, &thread_path);
                                break 'outer;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            break 'outer;
                        }
                    }
                }

                // Final flush on channel close. Instrumented (ag#8):
                // the wall-clock cost of this BufWriter::flush is the
                // dominant term in the rotation shutdown budget. Logging
                // it per-rotation gives us the data we need to tune
                // TRANSCRIPT_WRITER_SHUTDOWN_TIMEOUT against real p99.
                let flush_start = std::time::Instant::now();
                let flush_result = writer.flush();
                let flush_elapsed = flush_start.elapsed();
                if let Err(e) = flush_result {
                    io::handle_write_error(app_handle(), &thread_path, 0, 0, &e);
                    log::info!(
                        "transcript_writer.final_flush file={:?} elapsed_ms={} outcome=error",
                        thread_path,
                        flush_elapsed.as_millis()
                    );
                } else {
                    log::info!(
                        "transcript_writer.final_flush file={:?} elapsed_ms={} outcome=ok",
                        thread_path,
                        flush_elapsed.as_millis()
                    );
                }
                log::info!("Transcript writer: shut down for {:?}", thread_path);
            })
            .ok()?;

        Some(Self {
            tx,
            handle: Some(handle),
            shutdown_requested,
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
    /// Non-blocking: flips the shutdown flag and sends the `Shutdown` sentinel.
    /// The thread will exit on its own after flushing (and draining anything
    /// already queued). Use [`Self::shutdown_with_timeout`] when the caller
    /// needs bounded assurance that flush completed before moving on.
    ///
    /// Setting the flag before sending the message matters: a slow writer
    /// mid-`Append` checks the flag after the write lands and exits on the
    /// next tick instead of draining the whole queue first.
    pub fn shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::SeqCst);
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
        self.shutdown_requested.store(true, Ordering::SeqCst);
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
                //
                // Instrumentation (ag#8): time how long the join actually
                // takes. Combined with the writer-thread-side
                // `transcript_writer.final_flush elapsed_ms=…` line, this
                // gives us the full picture — caller-observed wall clock
                // vs. kernel-side flush cost. Once we have a couple of
                // weeks of field data, tune TRANSCRIPT_WRITER_SHUTDOWN_TIMEOUT
                // to p99(join) + safety margin.
                let join_start = std::time::Instant::now();
                let joined = done_rx.recv_timeout(timeout).is_ok();
                let elapsed = join_start.elapsed();
                log::info!(
                    "transcript_writer.shutdown_join elapsed_ms={} timeout_ms={} joined={}",
                    elapsed.as_millis(),
                    timeout.as_millis(),
                    joined
                );
                joined
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
/// a known-good file. I/O errors are classified via `io::handle_write_error`
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
use std::sync::{Mutex, RwLock};

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

// ---------------------------------------------------------------------------
// Tests — transcript writer shutdown contract (ag#7)
// ---------------------------------------------------------------------------
//
// These pin the behavior that matters for session rotation:
//   - `shutdown()` sets the atomic flag before sending the sentinel, so a
//     writer mid-drain observes the flag on the next `recv_timeout` tick and
//     exits instead of flushing the whole backlog.
//   - `drain_remaining` stops at `Shutdown` without over-consuming the channel.
//
// We test `drain_remaining` directly against a synthetic BufWriter (over a
// `Vec<u8>`-backed temp file) rather than going through `TranscriptWriter::spawn`,
// which would require HOME override and conflict with `sessions::usage::tests`
// under parallel execution.

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tempfile(label: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!(
            "audio-graph-shutdown-{}-{}-{}-{}",
            label, pid, nanos, n
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir.join("t.jsonl")
    }

    fn seg(id: &str, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: id.into(),
            source_id: "test".into(),
            speaker_id: None,
            speaker_label: None,
            text: text.into(),
            start_time: 0.0,
            end_time: 1.0,
            confidence: 1.0,
        }
    }

    #[test]
    fn drain_remaining_writes_pending_appends_then_stops() {
        // Simulates the writer hitting the shutdown flag mid-queue: the helper
        // must persist everything already in the channel so a caller-observed
        // shutdown doesn't silently drop buffered segments.
        let path = unique_tempfile("drain-pending");
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open temp file");
        let mut writer = BufWriter::new(file);

        let (tx, rx) = mpsc::channel::<TranscriptWriteMsg>();
        tx.send(TranscriptWriteMsg::Append(seg("a", "first")))
            .unwrap();
        tx.send(TranscriptWriteMsg::Append(seg("b", "second")))
            .unwrap();
        // Shutdown sentinel mid-queue — drain_remaining must stop here.
        tx.send(TranscriptWriteMsg::Shutdown).unwrap();
        // This one must NOT be written — it comes after the sentinel.
        tx.send(TranscriptWriteMsg::Append(seg("c", "after-sentinel")))
            .unwrap();

        drain_remaining(&rx, &mut writer, &path);
        writer.flush().unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("first"), "first segment must be written");
        assert!(
            contents.contains("second"),
            "second segment must be written"
        );
        assert!(
            !contents.contains("after-sentinel"),
            "drain must stop at Shutdown sentinel, got: {:?}",
            contents
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn drain_remaining_handles_empty_and_disconnected_channel() {
        // Boundary cases: an empty channel, and a channel whose sender is
        // already dropped. Neither should panic or block; both should simply
        // return with whatever BufWriter state the caller passed in.
        let path = unique_tempfile("drain-empty");
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open temp file");
        let mut writer = BufWriter::new(file);

        // Empty, still-open channel.
        let (tx, rx) = mpsc::channel::<TranscriptWriteMsg>();
        drain_remaining(&rx, &mut writer, &path);
        drop(tx);

        // Disconnected channel.
        let (tx2, rx2) = mpsc::channel::<TranscriptWriteMsg>();
        drop(tx2);
        drain_remaining(&rx2, &mut writer, &path);

        writer.flush().unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            contents.is_empty(),
            "no segments should be written, got: {:?}",
            contents
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn shutdown_sets_flag_before_sending_sentinel() {
        // Contract: `shutdown()` must flip `shutdown_requested` *before* the
        // sentinel lands in the channel, so a writer polling the flag on
        // recv_timeout observes shutdown even if it hasn't consumed the
        // sentinel yet. We stand up a fake TranscriptWriter (no real thread)
        // and assert flag state after the call — the send itself is covered
        // by the end-to-end `#[ignore]`d rotation tests in state.rs.
        let (tx, _rx) = mpsc::channel::<TranscriptWriteMsg>();
        let flag = Arc::new(AtomicBool::new(false));
        let writer = TranscriptWriter {
            tx,
            handle: None,
            shutdown_requested: flag.clone(),
        };
        assert!(!flag.load(Ordering::SeqCst));
        writer.shutdown();
        assert!(
            flag.load(Ordering::SeqCst),
            "shutdown() must set the shutdown_requested flag"
        );
    }
}
