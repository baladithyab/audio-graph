//! Lightweight session metadata index for cross-launch continuity.
//!
//! Maintains `~/.audiograph/sessions.json` — a small JSON array of session
//! descriptors that lets the UI browse past sessions without scanning the
//! transcript / graph directories on disk.
//!
//! The index is a *pointer* to the authoritative data files
//! (`transcripts/<uuid>.jsonl`, `graphs/<uuid>.json`); it is not the data
//! itself. If the index is corrupted or lost, sessions can still be recovered
//! by scanning those directories.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod usage;

/// Serializes read-modify-write access to `sessions.json` within this process.
///
/// Concurrent writers (e.g. the 30s graph-autosave tick calling `update_stats`
/// at the same instant `finalize_session` runs on shutdown, or an anomaly
/// where two threads race to register) would otherwise risk one overwriting
/// the other's changes because each does load→mutate→save. A process-local
/// mutex is sufficient: only one audio-graph process owns this file.
static INDEX_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub title: Option<String>,
    pub created_at: u64,       // unix millis
    pub ended_at: Option<u64>, // unix millis
    pub duration_seconds: Option<u64>,
    pub status: String, // "active" | "complete" | "crashed"
    pub segment_count: u64,
    pub speaker_count: u64,
    pub entity_count: u64,
    pub transcript_path: String,
    pub graph_path: String,
}

/// `~/.audiograph/sessions.json` (index file, not the data itself).
pub fn sessions_index_path() -> Result<PathBuf, String> {
    let base = dirs::home_dir().ok_or("cannot determine home dir")?;
    let dir = base.join(".audiograph");
    fs::create_dir_all(&dir).map_err(|e| format!("{}", e))?;
    Ok(dir.join("sessions.json"))
}

pub fn load_index() -> Vec<SessionMetadata> {
    match sessions_index_path() {
        Ok(path) if path.exists() => match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Vec::new(),
        },
        _ => Vec::new(),
    }
}

pub fn save_index(sessions: &[SessionMetadata]) -> Result<(), String> {
    let path = sessions_index_path()?;
    let json = serde_json::to_string_pretty(sessions).map_err(|e| format!("{}", e))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &json).map_err(|e| format!("{}", e))?;
    crate::fs_util::set_owner_only(&tmp);
    fs::rename(&tmp, &path).map_err(|e| format!("{}", e))?;
    crate::fs_util::set_owner_only(&path);
    Ok(())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Register current session in the index (called at app start).
pub fn register_session(session_id: &str) -> Result<(), String> {
    let _guard = INDEX_LOCK
        .lock()
        .map_err(|e| format!("index lock poisoned: {}", e))?;
    let mut index = load_index();
    // Mark any prior "active" sessions (from previous runs that didn't clean
    // up — e.g., SIGKILL, power loss) as "crashed". Skip the CURRENT session
    // id in the unlikely event register_session is called twice for the same
    // ID, which would otherwise cause the second call to self-crash.
    for entry in index.iter_mut() {
        if entry.status == "active" && entry.id != session_id {
            entry.status = "crashed".into();
            if entry.ended_at.is_none() {
                entry.ended_at = Some(now_millis());
            }
        }
    }
    let base = dirs::home_dir().ok_or("home dir")?.join(".audiograph");
    let meta = SessionMetadata {
        id: session_id.to_string(),
        title: None,
        created_at: now_millis(),
        ended_at: None,
        duration_seconds: None,
        status: "active".to_string(),
        segment_count: 0,
        speaker_count: 0,
        entity_count: 0,
        transcript_path: base
            .join("transcripts")
            .join(format!("{}.jsonl", session_id))
            .to_string_lossy()
            .to_string(),
        graph_path: base
            .join("graphs")
            .join(format!("{}.json", session_id))
            .to_string_lossy()
            .to_string(),
    };
    index.insert(0, meta);
    // Trim to 100 most recent
    if index.len() > 100 {
        index.truncate(100);
    }
    save_index(&index)
}

/// Update stats for current session.
pub fn update_stats(
    session_id: &str,
    segment_count: u64,
    speaker_count: u64,
    entity_count: u64,
) -> Result<(), String> {
    let _guard = INDEX_LOCK
        .lock()
        .map_err(|e| format!("index lock poisoned: {}", e))?;
    let mut index = load_index();
    if let Some(entry) = index.iter_mut().find(|e| e.id == session_id) {
        entry.segment_count = segment_count;
        entry.speaker_count = speaker_count;
        entry.entity_count = entity_count;
    }
    save_index(&index)
}

/// Remove a session from the index. Callers are responsible for deleting
/// the transcript/graph files on disk — this only touches the index.
pub fn remove_from_index(session_id: &str) -> Result<(), String> {
    let _guard = INDEX_LOCK
        .lock()
        .map_err(|e| format!("index lock poisoned: {}", e))?;
    let mut index = load_index();
    index.retain(|s| s.id != session_id);
    save_index(&index)
}

/// Mark session as complete on app shutdown.
pub fn finalize_session(session_id: &str) -> Result<(), String> {
    let _guard = INDEX_LOCK
        .lock()
        .map_err(|e| format!("index lock poisoned: {}", e))?;
    let mut index = load_index();
    if let Some(entry) = index.iter_mut().find(|e| e.id == session_id) {
        entry.status = "complete".into();
        let end = now_millis();
        entry.ended_at = Some(end);
        entry.duration_seconds = Some((end - entry.created_at) / 1000);
    }
    save_index(&index)
}
