//! Application settings — persistence layer for user configuration.
//!
//! Settings are stored as JSON in the app data directory and loaded
//! at startup. If the file is missing or unparseable, defaults are used.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::Manager;

// ---------------------------------------------------------------------------
// Helper default functions
// ---------------------------------------------------------------------------

fn default_aws_region() -> String {
    "us-east-1".to_string()
}
fn default_language_code() -> String {
    "en-US".to_string()
}
fn default_deepgram_model() -> String {
    "nova-3".to_string()
}
fn default_true() -> bool {
    true
}
fn default_sherpa_model() -> String {
    "streaming-zipformer-en-20M".to_string()
}

// ---------------------------------------------------------------------------
// AWS credential source
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AwsCredentialSource {
    #[serde(rename = "default_chain")]
    DefaultChain,
    #[serde(rename = "profile")]
    Profile { name: String },
    #[serde(rename = "access_keys")]
    AccessKeys { access_key: String },
}

impl Default for AwsCredentialSource {
    fn default() -> Self {
        Self::DefaultChain
    }
}

// ---------------------------------------------------------------------------
// ASR provider
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AsrProvider {
    #[serde(rename = "local_whisper")]
    LocalWhisper,
    #[serde(rename = "api")]
    Api {
        endpoint: String,
        api_key: String,
        model: String,
    },
    #[serde(rename = "aws_transcribe")]
    AwsTranscribe {
        #[serde(default = "default_aws_region")]
        region: String,
        #[serde(default = "default_language_code")]
        language_code: String,
        #[serde(default)]
        credential_source: AwsCredentialSource,
        #[serde(default = "default_true")]
        enable_diarization: bool,
    },
    #[serde(rename = "deepgram")]
    DeepgramStreaming {
        api_key: String,
        #[serde(default = "default_deepgram_model")]
        model: String,
        #[serde(default = "default_true")]
        enable_diarization: bool,
    },
    #[serde(rename = "assemblyai")]
    AssemblyAI {
        api_key: String,
        #[serde(default = "default_true")]
        enable_diarization: bool,
    },
    #[serde(rename = "sherpa_onnx")]
    SherpaOnnx {
        #[serde(default = "default_sherpa_model")]
        model_dir: String,
        #[serde(default = "default_true")]
        enable_endpoint_detection: bool,
    },
}

impl Default for AsrProvider {
    fn default() -> Self {
        Self::LocalWhisper
    }
}

// ---------------------------------------------------------------------------
// LLM API config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmApiConfig {
    pub endpoint: String,
    #[serde(default)]
    pub api_key: Option<String>,
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

fn default_max_tokens() -> u32 {
    2048
}
fn default_temperature() -> f32 {
    0.7
}

// ---------------------------------------------------------------------------
// LLM provider
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LlmProvider {
    #[serde(rename = "local_llama")]
    LocalLlama,
    #[serde(rename = "api")]
    Api {
        endpoint: String,
        api_key: String,
        model: String,
    },
    #[serde(rename = "aws_bedrock")]
    AwsBedrock {
        #[serde(default = "default_aws_region")]
        region: String,
        model_id: String,
        #[serde(default)]
        credential_source: AwsCredentialSource,
    },
    #[serde(rename = "mistralrs")]
    MistralRs {
        #[serde(default = "default_mistralrs_model")]
        model_id: String,
    },
}

fn default_mistralrs_model() -> String {
    "ggml-small-extract.gguf".to_string()
}

impl Default for LlmProvider {
    fn default() -> Self {
        Self::Api {
            endpoint: "http://localhost:11434/v1".to_string(),
            api_key: String::new(),
            model: "llama3.2".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Audio settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSettings {
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_channels")]
    pub channels: u16,
}

fn default_sample_rate() -> u32 {
    16000
}
fn default_channels() -> u16 {
    1
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            sample_rate: default_sample_rate(),
            channels: default_channels(),
        }
    }
}

// ---------------------------------------------------------------------------
// Gemini auth mode + settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GeminiAuthMode {
    #[serde(rename = "api_key")]
    ApiKey { api_key: String },
    #[serde(rename = "vertex_ai")]
    VertexAI {
        project_id: String,
        location: String,
        #[serde(default)]
        service_account_path: Option<String>,
    },
}

impl Default for GeminiAuthMode {
    fn default() -> Self {
        Self::ApiKey {
            api_key: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiSettings {
    #[serde(default)]
    pub auth: GeminiAuthMode,
    #[serde(default = "default_gemini_model")]
    pub model: String,
}

fn default_gemini_model() -> String {
    "gemini-3.1-flash-live-preview".to_string()
}

impl Default for GeminiSettings {
    fn default() -> Self {
        Self {
            auth: GeminiAuthMode::default(),
            model: default_gemini_model(),
        }
    }
}

impl GeminiSettings {
    /// Extract the API key from auth mode (convenience for backward compat).
    pub fn api_key(&self) -> String {
        match &self.auth {
            GeminiAuthMode::ApiKey { api_key } => api_key.clone(),
            GeminiAuthMode::VertexAI { .. } => String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub asr_provider: AsrProvider,
    #[serde(default = "default_whisper_model")]
    pub whisper_model: String,
    #[serde(default)]
    pub llm_provider: LlmProvider,
    #[serde(default)]
    pub llm_api_config: Option<LlmApiConfig>,
    #[serde(default)]
    pub audio_settings: AudioSettings,
    #[serde(default)]
    pub gemini: GeminiSettings,
}

fn default_whisper_model() -> String {
    "ggml-small.en.bin".to_string()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            asr_provider: AsrProvider::default(),
            whisper_model: default_whisper_model(),
            llm_provider: LlmProvider::default(),
            llm_api_config: None,
            audio_settings: AudioSettings::default(),
            gemini: GeminiSettings::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

pub fn get_settings_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {}", e))?;
    Ok(data_dir.join("settings.json"))
}

// ---------------------------------------------------------------------------
// Load / Save
// ---------------------------------------------------------------------------

pub fn load_settings(app: &tauri::AppHandle) -> AppSettings {
    match get_settings_path(app) {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str::<AppSettings>(&contents) {
                        Ok(settings) => {
                            log::info!("Loaded settings from {}", path.display());
                            settings
                        }
                        Err(e) => {
                            log::warn!("Failed to parse settings file, using defaults: {}", e);
                            AppSettings::default()
                        }
                    },
                    Err(e) => {
                        log::warn!("Failed to read settings file, using defaults: {}", e);
                        AppSettings::default()
                    }
                }
            } else {
                log::info!("No settings file found, using defaults");
                AppSettings::default()
            }
        }
        Err(e) => {
            log::warn!("Failed to determine settings path, using defaults: {}", e);
            AppSettings::default()
        }
    }
}

pub fn save_settings(app: &tauri::AppHandle, settings: &AppSettings) -> Result<(), String> {
    let path = get_settings_path(app)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create settings directory: {}", e))?;
    }

    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;

    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, &json).map_err(|e| format!("Failed to write settings file: {}", e))?;

    // Lock down perms before rename so the file is never world-readable, even briefly.
    crate::fs_util::set_owner_only(&tmp_path);

    fs::rename(&tmp_path, &path).map_err(|e| format!("Failed to finalize settings file: {}", e))?;

    // Re-apply after rename in case rename semantics differ across platforms.
    crate::fs_util::set_owner_only(&path);

    log::info!("Settings saved to {}", path.display());
    Ok(())
}
