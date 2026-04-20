//! Per-session Gemini token-usage persistence.
//!
//! Before loop 19, token usage + turn counts for each Gemini Live session
//! lived only in the frontend's `localStorage` (`TokenUsagePanel.tsx` keys
//! `tokens.session.v1` / `tokens.lifetime.v1`). That meant a crash, a
//! reinstall, or clearing browser storage lost the history.
//!
//! This module mirrors the frontend accumulator on disk at
//! `~/.audiograph/usage/<session_id>.json` so token totals survive app
//! restarts. The file is small (< 1 KB), bounded by the struct fields, and
//! written atomically via tmp-file + rename.
//!
//! The authoritative write site is the Gemini `TurnComplete` handler in
//! `commands.rs`, which calls [`append_turn`] once per model turn. The
//! session index (`sessions.json`) stays the pointer; this file holds the
//! numbers.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Serializes per-session usage file read-modify-write so two concurrent
/// `append_turn` calls (e.g. a TurnComplete landing during `new_session_cmd`)
/// can't lose an increment. One mutex for all sessions is fine: writes are
/// tiny and rare (≤ a few Hz).
static USAGE_LOCK: Mutex<()> = Mutex::new(());

/// Cumulative token totals and turn count for a single session.
///
/// Field names match the frontend's `Totals` shape in
/// `TokenUsagePanel.tsx` so a later loop can wire the backend copy straight
/// into the UI without a translation layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub session_id: String,
    pub prompt: u64,
    pub response: u64,
    pub cached: u64,
    pub thoughts: u64,
    pub tool_use: u64,
    pub total: u64,
    pub turns: u64,
    /// Unix millis of the last update. `0` means never updated.
    pub updated_at: u64,
}

/// Counter delta for a single Gemini turn. All fields default to 0 so callers
/// only need to fill in what the server sent.
#[derive(Debug, Clone, Copy, Default)]
pub struct TurnDelta {
    pub prompt: u64,
    pub response: u64,
    pub cached: u64,
    pub thoughts: u64,
    pub tool_use: u64,
    pub total: u64,
}

/// Root directory for usage files (`~/.audiograph/usage/`). Creates parents.
pub fn usage_dir() -> Result<PathBuf, String> {
    let base = base_data_dir().ok_or("cannot determine home dir")?;
    let dir = base.join("usage");
    fs::create_dir_all(&dir).map_err(|e| format!("create {:?}: {}", dir, e))?;
    Ok(dir)
}

/// Path to a specific session's usage file.
pub fn usage_path(session_id: &str) -> Result<PathBuf, String> {
    Ok(usage_dir()?.join(format!("{}.json", session_id)))
}

fn base_data_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    let home = std::env::var("HOME").ok();
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").ok();
    #[cfg(not(any(unix, windows)))]
    let home: Option<String> = None;
    home.map(|h| PathBuf::from(h).join(".audiograph"))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Load the usage record for `session_id`. Missing file → zeroed record.
/// Malformed file → zeroed record (logged at `warn`); we never propagate a
/// parse error up the stack because a corrupt usage file must not block a
/// live capture.
pub fn load_usage(session_id: &str) -> SessionUsage {
    let path = match usage_path(session_id) {
        Ok(p) => p,
        Err(_) => return zeroed(session_id),
    };
    if !path.exists() {
        return zeroed(session_id);
    }
    match fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str::<SessionUsage>(&contents).unwrap_or_else(|e| {
            log::warn!("usage: malformed {:?} ({}), resetting to zero", path, e);
            zeroed(session_id)
        }),
        Err(e) => {
            log::warn!("usage: read {:?} failed: {}", path, e);
            zeroed(session_id)
        }
    }
}

fn zeroed(session_id: &str) -> SessionUsage {
    SessionUsage {
        session_id: session_id.to_string(),
        ..SessionUsage::default()
    }
}

/// Overwrite the usage file for a session with `usage`. Atomic: writes to a
/// sibling `.tmp` file and renames. Sets owner-only permissions on both.
pub fn save_usage(usage: &SessionUsage) -> Result<(), String> {
    let path = usage_path(&usage.session_id)?;
    let json = serde_json::to_string_pretty(usage).map_err(|e| format!("{}", e))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &json).map_err(|e| format!("write {:?}: {}", tmp, e))?;
    crate::fs_util::set_owner_only(&tmp);
    fs::rename(&tmp, &path).map_err(|e| format!("rename {:?} -> {:?}: {}", tmp, path, e))?;
    crate::fs_util::set_owner_only(&path);
    Ok(())
}

/// Add one turn's counters to the session's on-disk totals. Uses
/// saturating-add so a runaway provider counter can never wrap into zero.
pub fn append_turn(session_id: &str, delta: TurnDelta) -> Result<SessionUsage, String> {
    let _guard = USAGE_LOCK
        .lock()
        .map_err(|e| format!("usage lock poisoned: {}", e))?;
    let mut u = load_usage(session_id);
    u.session_id = session_id.to_string();
    u.prompt = u.prompt.saturating_add(delta.prompt);
    u.response = u.response.saturating_add(delta.response);
    u.cached = u.cached.saturating_add(delta.cached);
    u.thoughts = u.thoughts.saturating_add(delta.thoughts);
    u.tool_use = u.tool_use.saturating_add(delta.tool_use);
    u.total = u.total.saturating_add(delta.total);
    u.turns = u.turns.saturating_add(1);
    u.updated_at = now_millis();
    save_usage(&u)?;
    Ok(u)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// Integration-style round-trip tests use `HOME` override so we hit a tempdir
// instead of `~/.audiograph`. The usage file helpers resolve the base dir
// lazily on each call, so setting `HOME` just before the test works.
//
// NB: the tests are serialized via `USAGE_TEST_LOCK` so one test's HOME
// override doesn't race another's. `cargo test` runs threaded by default.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex as StdMutex;

    static USAGE_TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn unique_tempdir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!(
            "audio-graph-usage-{}-{}-{}-{}",
            label, pid, nanos, n
        ));
        fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    /// Set HOME / USERPROFILE for the duration of a test. Returns a guard
    /// that restores the original value on drop.
    struct HomeGuard {
        prev_home: Option<String>,
        prev_userprofile: Option<String>,
    }

    impl HomeGuard {
        #[allow(unsafe_code)] // env mutation, safe while USAGE_TEST_LOCK is held
        fn set(dir: &std::path::Path) -> Self {
            let prev_home = std::env::var("HOME").ok();
            let prev_userprofile = std::env::var("USERPROFILE").ok();
            // SAFETY: serialized by USAGE_TEST_LOCK; no other thread reads env.
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
            // SAFETY: serialized by USAGE_TEST_LOCK.
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
    fn missing_file_loads_as_zero_usage() {
        let _lock = USAGE_TEST_LOCK.lock().unwrap();
        let dir = unique_tempdir("missing");
        let _g = HomeGuard::set(&dir);

        let u = load_usage("never-seen");
        assert_eq!(u.session_id, "never-seen");
        assert_eq!(u.turns, 0);
        assert_eq!(u.total, 0);
        assert_eq!(u.updated_at, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_turn_round_trips_through_disk() {
        let _lock = USAGE_TEST_LOCK.lock().unwrap();
        let dir = unique_tempdir("roundtrip");
        let _g = HomeGuard::set(&dir);

        let sid = "test-session-abc";
        let delta = TurnDelta {
            prompt: 100,
            response: 50,
            cached: 10,
            thoughts: 5,
            tool_use: 2,
            total: 167,
        };

        let after_first = append_turn(sid, delta).expect("first append");
        assert_eq!(after_first.turns, 1);
        assert_eq!(after_first.prompt, 100);
        assert_eq!(after_first.total, 167);
        assert!(after_first.updated_at > 0);

        // Round-trip: load freshly from disk, values must match the
        // in-memory record returned by append_turn.
        let from_disk = load_usage(sid);
        assert_eq!(from_disk, after_first);

        // Second append accumulates — this is the core invariant the
        // frontend's localStorage version has.
        let after_second = append_turn(sid, delta).expect("second append");
        assert_eq!(after_second.turns, 2);
        assert_eq!(after_second.prompt, 200);
        assert_eq!(after_second.total, 334);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_file_recovers_to_zero() {
        let _lock = USAGE_TEST_LOCK.lock().unwrap();
        let dir = unique_tempdir("malformed");
        let _g = HomeGuard::set(&dir);

        let sid = "broken-session";
        let path = usage_path(sid).expect("usage path");
        fs::write(&path, b"this is not json").unwrap();

        // A malformed file must not panic and must not block future writes.
        // Callers get a zeroed record back.
        let u = load_usage(sid);
        assert_eq!(u.turns, 0);
        assert_eq!(u.total, 0);

        // A subsequent append_turn overwrites the garbage file cleanly.
        let delta = TurnDelta {
            total: 42,
            ..TurnDelta::default()
        };
        let after = append_turn(sid, delta).expect("append after malformed");
        assert_eq!(after.turns, 1);
        assert_eq!(after.total, 42);

        let reloaded = load_usage(sid);
        assert_eq!(reloaded, after);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saturating_add_prevents_overflow() {
        let _lock = USAGE_TEST_LOCK.lock().unwrap();
        let dir = unique_tempdir("saturate");
        let _g = HomeGuard::set(&dir);

        // Seed the file near u64::MAX. A subsequent append must clamp to
        // MAX instead of wrapping to zero — which would erase the history
        // we're trying to preserve.
        let sid = "saturating-session";
        let seed = SessionUsage {
            session_id: sid.to_string(),
            total: u64::MAX - 1,
            ..SessionUsage::default()
        };
        save_usage(&seed).unwrap();

        let delta = TurnDelta {
            total: 10,
            ..TurnDelta::default()
        };
        let after = append_turn(sid, delta).unwrap();
        assert_eq!(after.total, u64::MAX);
        assert_eq!(after.turns, 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
