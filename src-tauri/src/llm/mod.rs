//! LLM inference backends.
//!
//! Three backends are available:
//! - **Native** (`engine`): In-process GGUF model inference via llama-cpp-2.
//! - **API** (`api_client`): OpenAI-compatible HTTP API (OpenAI, Ollama, LM Studio, vLLM, etc.).
//! - **MistralRs** (`mistralrs_engine`): Rust-native GGUF inference via mistral.rs (Candle),
//!   with JSON Schema-constrained structured generation for entity extraction.
//!
//! The speech processor and chat commands try the user's preferred backend first,
//! then fallback alternatives, then rule-based extraction as a final fallback.

pub mod api_client;
pub mod engine;
pub mod mistralrs_engine;

pub use api_client::{ApiClient, ApiConfig};
pub use engine::LlmEngine;
pub use mistralrs_engine::MistralRsEngine;
