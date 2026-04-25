//! Credential management — stores API keys in ~/.config/audio-graph/credentials.yaml.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Canonical list of credential keys accepted by `save_credential_cmd`,
/// `load_credential_cmd`, and `delete_credential_cmd`. This is the boundary
/// allowlist — `set_field` below performs the inner-layer match, but commands
/// should reject unknown keys up front using [`is_allowed_key`].
///
/// IMPORTANT: this must stay in sync with the frontend constant
/// `ALLOWED_CREDENTIAL_KEYS` in `src/types/index.ts` and with the match arms
/// in `set_field` / `load_credential_cmd`.
pub const ALLOWED_CREDENTIAL_KEYS: &[&str] = &[
    "openai_api_key",
    "groq_api_key",
    "together_api_key",
    "fireworks_api_key",
    "deepgram_api_key",
    "assemblyai_api_key",
    "gemini_api_key",
    "google_service_account_path",
    "aws_access_key",
    "aws_secret_key",
    "aws_session_token",
    "aws_profile",
    "aws_region",
];

/// Returns `true` if `key` is a recognized credential field name.
pub fn is_allowed_key(key: &str) -> bool {
    ALLOWED_CREDENTIAL_KEYS.contains(&key)
}

/// Stores API credentials for cloud providers.
///
/// # Security
///
/// This type derives [`Zeroize`] and [`ZeroizeOnDrop`] so that all secret
/// fields are overwritten with zeros when the struct goes out of scope.
/// This mitigates exposure of plaintext API keys in memory dumps, swap
/// files, and cold-boot attacks. The `serde` feature of the `zeroize`
/// crate makes the derive compatible with the existing `Serialize`/
/// `Deserialize` implementations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct CredentialStore {
    // --- OpenAI-compatible API keys ---
    #[serde(default)]
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub groq_api_key: Option<String>,
    #[serde(default)]
    pub together_api_key: Option<String>,
    #[serde(default)]
    pub fireworks_api_key: Option<String>,

    // --- Streaming ASR provider keys ---
    #[serde(default)]
    pub deepgram_api_key: Option<String>,
    #[serde(default)]
    pub assemblyai_api_key: Option<String>,

    // --- Google ---
    #[serde(default)]
    pub gemini_api_key: Option<String>,
    #[serde(default)]
    pub google_service_account_path: Option<String>,

    // --- AWS ---
    #[serde(default)]
    pub aws_access_key: Option<String>,
    #[serde(default)]
    pub aws_secret_key: Option<String>,
    #[serde(default)]
    pub aws_session_token: Option<String>,
    #[serde(default)]
    pub aws_profile: Option<String>,
    #[serde(default)]
    pub aws_region: Option<String>,
}

pub fn config_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or_else(|| "Cannot determine config directory".to_string())?;
    let dir = base.join("audio-graph");
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create config dir: {}", e))?;
    Ok(dir)
}

pub fn credentials_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("credentials.yaml"))
}

pub fn load_credentials() -> CredentialStore {
    match credentials_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_yaml::from_str::<CredentialStore>(&contents) {
                        Ok(store) => store,
                        Err(e) => {
                            log::error!(
                                    "Failed to parse credentials.yaml ({}): using empty credential store. \
                                     Backup your file and re-enter credentials in Settings.",
                                    e
                                );
                            CredentialStore::default()
                        }
                    },
                    Err(e) => {
                        log::error!(
                            "Failed to read credentials.yaml ({}): using empty credential store.",
                            e
                        );
                        CredentialStore::default()
                    }
                }
            } else {
                // File doesn't exist — this is normal on first run, not an error.
                CredentialStore::default()
            }
        }
        Err(e) => {
            log::warn!("Cannot locate config directory for credentials: {}", e);
            CredentialStore::default()
        }
    }
}

/// Load credentials with detailed error reporting.
/// Returns `Ok(store)` for success (including the missing-file case with an
/// empty store), and `Err(reason)` only when the file exists but cannot be
/// parsed or read.
pub fn try_load_credentials() -> Result<CredentialStore, String> {
    let path = credentials_path()?;
    if !path.exists() {
        return Ok(CredentialStore::default());
    }
    let contents = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    serde_yaml::from_str::<CredentialStore>(&contents)
        .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
}

pub fn save_credentials(store: &CredentialStore) -> Result<(), String> {
    let path = credentials_path()?;
    let yaml = serde_yaml::to_string(store)
        .map_err(|e| format!("Failed to serialize credentials: {}", e))?;
    let tmp_path = path.with_extension("yaml.tmp");
    fs::write(&tmp_path, &yaml).map_err(|e| format!("Failed to write credentials: {}", e))?;

    // Set restrictive permissions on the tmp file before rename, in case the
    // rename preserves the source file's permissions on some platforms.
    crate::fs_util::set_owner_only(&tmp_path);

    fs::rename(&tmp_path, &path).map_err(|e| format!("Failed to finalize credentials: {}", e))?;

    // And again on the final file to be safe.
    crate::fs_util::set_owner_only(&path);

    log::info!("Credentials saved to {}", path.display());
    Ok(())
}

fn set_field(store: &mut CredentialStore, key: &str, value: Option<String>) -> Result<(), String> {
    match key {
        "openai_api_key" => store.openai_api_key = value,
        "groq_api_key" => store.groq_api_key = value,
        "together_api_key" => store.together_api_key = value,
        "fireworks_api_key" => store.fireworks_api_key = value,
        "deepgram_api_key" => store.deepgram_api_key = value,
        "assemblyai_api_key" => store.assemblyai_api_key = value,
        "gemini_api_key" => store.gemini_api_key = value,
        "google_service_account_path" => store.google_service_account_path = value,
        "aws_access_key" => store.aws_access_key = value,
        "aws_secret_key" => store.aws_secret_key = value,
        "aws_session_token" => store.aws_session_token = value,
        "aws_profile" => store.aws_profile = value,
        "aws_region" => store.aws_region = value,
        _ => return Err(format!("Unknown credential key: {}", key)),
    }
    Ok(())
}

pub fn set_credential(key: &str, value: &str) -> Result<(), String> {
    // Empty (or whitespace-only) values are treated as "delete" to prevent
    // accidentally clobbering a valid stored credential when a user leaves a
    // form field blank after it was pre-populated from disk. Callers that
    // actually want to clear a credential should use `delete_credential`.
    let trimmed = value.trim();
    if trimmed.is_empty() {
        log::debug!(
            "set_credential({key}): value is empty/whitespace — skipping (use delete_credential to clear)"
        );
        return Ok(());
    }
    let mut store = load_credentials();
    set_field(&mut store, key, Some(trimmed.to_string()))?;
    save_credentials(&store)
}

pub fn delete_credential(key: &str) -> Result<(), String> {
    let mut store = load_credentials();
    set_field(&mut store, key, None)?;
    save_credentials(&store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_allowed_key_accepts_known_credential_name() {
        assert!(is_allowed_key("openai_api_key"));
    }

    #[test]
    fn is_allowed_key_rejects_unknown_key_and_path_traversal_attempts() {
        assert!(!is_allowed_key("not_a_real_key"));
        assert!(!is_allowed_key(""));
        assert!(!is_allowed_key("../etc/passwd"));
    }
}
