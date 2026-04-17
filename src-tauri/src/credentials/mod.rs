//! Credential management — stores API keys in ~/.config/audio-graph/credentials.yaml.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use zeroize::{Zeroize, ZeroizeOnDrop};

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
    let base =
        dirs::config_dir().ok_or_else(|| "Cannot determine config directory".to_string())?;
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
                    Ok(contents) => serde_yaml::from_str(&contents).unwrap_or_default(),
                    Err(_) => CredentialStore::default(),
                }
            } else {
                CredentialStore::default()
            }
        }
        Err(_) => CredentialStore::default(),
    }
}

pub fn save_credentials(store: &CredentialStore) -> Result<(), String> {
    let path = credentials_path()?;
    let yaml =
        serde_yaml::to_string(store).map_err(|e| format!("Failed to serialize credentials: {}", e))?;
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
    let mut store = load_credentials();
    set_field(&mut store, key, Some(value.to_string()))?;
    save_credentials(&store)
}

pub fn delete_credential(key: &str) -> Result<(), String> {
    let mut store = load_credentials();
    set_field(&mut store, key, None)?;
    save_credentials(&store)
}
