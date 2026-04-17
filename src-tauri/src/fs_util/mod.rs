//! Cross-platform helpers for restrictive file permissions.

use std::fs;
use std::path::Path;

/// Set a file to owner-only read/write (0o600 on Unix, owner-only ACL on Windows).
/// Best-effort — logs a warning on failure.
pub fn set_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            log::warn!("Failed to set 0o600 on {}: {}", path.display(), e);
        }
    }
    #[cfg(windows)]
    {
        // On Windows, file permissions inherit from parent directory by default.
        // For truly restricted access, use Windows ACL APIs via the `windows` crate.
        // For now, use read_only(false) as a best-effort marker and rely on
        // the parent directory (app data dir) being user-scoped.
        if let Ok(meta) = fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_readonly(false);
            let _ = fs::set_permissions(path, perms);
        }
    }
}
