# Provider Architecture: Local + Cloud Alternatives at Every Pipeline Stage

**Date:** 2026-04-16
**Status:** IMPLEMENTED

## Overview

Every pipeline stage in audio-graph supports swappable local and cloud providers.
The user selects providers in the Settings UI. Credentials are stored securely
in `~/.config/audio-graph/credentials.yaml` (chmod 600 on Unix). Non-sensitive
settings (provider type, region, model names) live in `settings.json`.

## Pipeline Stages and Providers

### 1. ASR (Automatic Speech Recognition)

| Provider | Type | Protocol | Diarization | Latency | Cost | Status |
|----------|------|----------|-------------|---------|------|--------|
| **Local Whisper** | Local | whisper-rs + Metal/CUDA | No (separate) | ~500-2000ms | Free | DONE |
| **OpenAI-compatible API** | Cloud/Batch | HTTP multipart | No | ~200-3000ms + 2s accum | Varies | DONE |
| **AWS Transcribe Streaming** | Cloud/Stream | HTTP/2 (SDK) | Yes (built-in) | ~200-500ms partial | $0.024/min | DONE |
| **Deepgram** | Cloud/Stream | WebSocket | Yes (built-in) | ~300-800ms | $0.0077/min | DONE |
| **AssemblyAI** | Cloud/Stream | WebSocket | Yes (built-in) | ~300-800ms | $0.012/min | DONE |
| **SherpaOnnx** | Local | ONNX Zipformer | Yes (streaming) | ~200ms | Free | DONE |

**Settings enum (implemented in `settings/mod.rs`):**
```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AsrProvider {
    #[serde(rename = "local_whisper")]
    LocalWhisper,
    #[serde(rename = "api")]
    Api { endpoint: String, api_key: String, model: String },
    #[serde(rename = "aws_transcribe")]
    AwsTranscribe { region: String, language_code: String, credential_source: AwsCredentialSource, enable_diarization: bool },
    #[serde(rename = "deepgram")]
    DeepgramStreaming { api_key: String, model: String, enable_diarization: bool },
    #[serde(rename = "assemblyai")]
    AssemblyAI { api_key: String, enable_diarization: bool },
}
```

### 2. LLM / Entity Extraction

| Provider | Type | Protocol | Notes | Status |
|----------|------|----------|-------|--------|
| **Local llama.cpp** | Local | llama-cpp-2 | GBNF grammar-constrained, Metal GPU | DONE |
| **OpenAI-compatible API** | Cloud | HTTP JSON | Ollama, OpenAI, Groq, Together, etc. | DONE |
| **AWS Bedrock** | Cloud | HTTP (SDK) | Claude, Llama, Mistral via AWS | DONE |
| **mistral.rs (Candle)** | Local | In-process GGUF | JSON Schema structured output | DONE |

**Settings enum (implemented in `settings/mod.rs`):**
```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LlmProvider {
    #[serde(rename = "local_llama")]
    LocalLlama,
    #[serde(rename = "api")]
    Api { endpoint: String, api_key: String, model: String },
    #[serde(rename = "aws_bedrock")]
    AwsBedrock { region: String, model_id: String, credential_source: AwsCredentialSource },
}
```

### 3. Full Pipeline (Speech + Extraction combined)

| Provider | Type | Protocol | Notes | Status |
|----------|------|----------|-------|--------|
| **Custom Speech Processor** | Local+Cloud mix | Internal | ASR + Diarization + LLM extraction | DONE |
| **Gemini Live** | Cloud | WebSocket | Streaming transcription + model responses | DONE |

### 4. Gemini Authentication

| Auth Mode | Use Case | Mechanism | Status |
|-----------|----------|-----------|--------|
| **AI Studio API Key** | Developer/consumer | Query param `?key=` | DONE |
| **Vertex AI** | Enterprise/GCP | Bearer token (gcp_auth) | DONE |

**Settings (implemented in `settings/mod.rs`):**
```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GeminiAuthMode {
    #[serde(rename = "api_key")]
    ApiKey { api_key: String },
    #[serde(rename = "vertex_ai")]
    VertexAI { project_id: String, location: String, service_account_path: Option<String> },
}
```

## Credential Management

### Implementation

Credentials are stored in `~/.config/audio-graph/credentials.yaml` via the
`credentials/mod.rs` module. The `CredentialStore` struct holds optional fields
for each provider's API keys and secrets.

**Implemented Tauri commands:**
- `save_credential_cmd(key, value)` -- Upserts a credential
- `load_credential_cmd(key)` -- Returns a single credential
- `load_all_credentials_cmd()` -- Returns the entire store
- `list_aws_profiles()` -- Parses `~/.aws/config` for profile names

### AWS Credentials

Three modes offered (all implemented):

| Mode | Description | Storage | Status |
|------|-------------|---------|--------|
| **DefaultChain** | Auto-detect env, profiles, SSO | Nothing stored | DONE |
| **Profile** | Named AWS profile | Profile name in settings.json | DONE |
| **AccessKeys** | Manual access key + secret | credentials.yaml | DONE |

### Google Credentials

| Mode | Description | Storage | Status |
|------|-------------|---------|--------|
| **AI Studio API Key** | Single API key | credentials.yaml | DONE |
| **Vertex AI ADC** | `gcloud auth application-default login` | Nothing stored | DONE |
| **Vertex AI Service Account** | Path to SA JSON | Path in settings.json | DONE |

## Dependencies (all added to Cargo.toml)

```toml
# AWS SDK (always compiled)
aws-config = { version = "1.1", features = ["behavior-version-latest", "sso"] }
aws-sdk-transcribestreaming = "1.102"
aws-credential-types = "1"
aws-sdk-sts = "1.101"
aws-smithy-http = "0.63"
tokio-stream = "0.1"

# Google Cloud auth (Vertex AI)
gcp_auth = "0.12"

# WebSocket (Gemini, Deepgram, AssemblyAI)
tokio-tungstenite = { version = "0.29", features = ["native-tls"] }

# Credential storage
serde_yaml = "0.9"
dirs = "6"

# HTTP multipart (cloud ASR API)
reqwest = { version = "0.13.2", features = ["blocking", "json", "multipart"] }
```

## Implementation Status

### Phase 1: Foundations -- DONE
- [x] Credential management module (`credentials/mod.rs`)
- [x] Settings module with all provider enums (`settings/mod.rs`)
- [x] Cloud ASR via HTTP multipart (`asr/cloud.rs`)
- [x] Settings load/save Tauri commands

### Phase 2: AWS Integration -- DONE
- [x] AWS SDK dependencies (always compiled, not feature-gated)
- [x] AWS Transcribe streaming worker (`asr/aws_transcribe.rs`)
- [x] AWS credential management (DefaultChain, Profile, AccessKeys)
- [x] `list_aws_profiles` command

### Phase 3: Vertex AI + Gemini Auth -- DONE
- [x] `gcp_auth` dependency
- [x] Gemini Live client with API Key and Vertex AI auth modes (`gemini/mod.rs`)
- [x] GeminiAuthMode enum in settings

### Phase 4: Streaming ASR Providers -- DONE
- [x] Deepgram WebSocket streaming client (`asr/deepgram.rs`)
- [x] AssemblyAI WebSocket streaming client (`asr/assemblyai.rs`)
- [x] Both providers added to AsrProvider enum and speech processor dispatch

### Phase 5: LLM Providers -- DONE
- [x] Local llama.cpp with GBNF grammar (`llm/engine.rs`)
- [x] OpenAI-compatible API client (`llm/api_client.rs`)
- [x] AWS Bedrock support via LlmProvider::AwsBedrock
- [x] Extraction chain fallback logic in speech processor

### Phase 6: Local Streaming + Advanced LLM -- DONE
- [x] sherpa-onnx streaming ASR via Zipformer transducer (asr/sherpa_streaming.rs)
- [x] mistral.rs LLM with JSON Schema-constrained structured generation (llm/mistralrs_engine.rs)
- [x] Whisper model size picker (5 sizes: tiny, base, small, medium, large-v3)
