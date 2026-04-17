//! LLM inference engine backed by mistral.rs (Candle).
//!
//! Uses mistral.rs for in-process GGUF model inference with structured
//! generation via JSON Schema constraints.  Unlike llama-cpp-2, the
//! mistral.rs `Model` type is `Send + Sync`, so the engine can be shared
//! across threads without creating per-call contexts.
//!
//! Entity extraction uses [`Model::generate_structured`] which derives
//! a JSON Schema from [`ExtractionResult`]'s `schemars::JsonSchema`
//! implementation and constrains the model output automatically.

use std::sync::Arc;

use mistralrs::{GgufModelBuilder, Model, TextMessageRole, TextMessages};

use crate::graph::entities::ExtractionResult;
use crate::llm::engine::ChatMessage;

// ---------------------------------------------------------------------------
// MistralRsEngine
// ---------------------------------------------------------------------------

/// Native LLM engine using mistral.rs (Candle) for GGUF model inference.
///
/// `Model` is `Send + Sync` so this engine can live in shared state without
/// per-call context creation (unlike `LlmEngine` which wraps llama-cpp-2).
///
/// A dedicated tokio runtime is stored alongside the model to bridge
/// async mistral.rs calls into the synchronous speech-processor threads.
pub struct MistralRsEngine {
    model: Model,
    rt: Arc<tokio::runtime::Runtime>,
}

impl MistralRsEngine {
    /// Load a GGUF model from disk (blocking).
    ///
    /// Creates a dedicated tokio runtime for async model loading and
    /// subsequent inference calls.  Use this when calling from
    /// synchronous code (e.g., speech processor initialization threads).
    ///
    /// `model_dir` is the directory containing the model file(s).
    /// `model_filename` is the GGUF file name within that directory.
    pub fn new(model_dir: &str, model_filename: &str) -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("mistralrs-rt")
            .build()
            .map_err(|e| format!("Failed to create tokio runtime for mistral.rs: {}", e))?;

        let model = rt
            .block_on(GgufModelBuilder::new(model_dir, vec![model_filename.to_string()]).build())
            .map_err(|e| format!("Failed to build mistral.rs model: {}", e))?;

        log::info!(
            "mistral.rs model loaded from: {}/{}",
            model_dir,
            model_filename
        );

        Ok(Self {
            model,
            rt: Arc::new(rt),
        })
    }

    /// Check if model is loaded and ready.
    pub fn is_loaded(&self) -> bool {
        true // If we constructed successfully, model is loaded
    }

    // ------------------------------------------------------------------
    // Entity extraction (JSON Schema-constrained structured generation)
    // ------------------------------------------------------------------

    /// Extract entities and relations from text using JSON Schema-constrained
    /// structured generation.
    ///
    /// Uses [`Model::generate_structured`] which automatically derives the
    /// JSON Schema from [`ExtractionResult`]'s `schemars::JsonSchema`
    /// implementation, constraining the model to produce valid JSON that
    /// deserializes without error.
    pub fn extract_entities(&self, text: &str, speaker: &str) -> Result<ExtractionResult, String> {
        let prompt = format!(
            r#"Extract entities and relationships from this conversation segment.
Output valid JSON matching the schema.

Speaker: {}
Text: {}

If no entities are found, return {{"entities": [], "relations": []}}.
Output JSON:"#,
            speaker, text
        );

        let messages = TextMessages::new().add_message(TextMessageRole::User, &prompt);

        let result: ExtractionResult = self
            .rt
            .block_on(self.model.generate_structured::<ExtractionResult>(messages))
            .map_err(|e| format!("mistral.rs structured extraction failed: {}", e))?;

        log::debug!(
            "mistral.rs extraction: {} entities, {} relations",
            result.entities.len(),
            result.relations.len()
        );

        Ok(result)
    }

    // ------------------------------------------------------------------
    // Chat
    // ------------------------------------------------------------------

    /// Chat with the LLM, providing graph context in the system prompt.
    pub fn chat(&self, messages: &[ChatMessage], graph_context: &str) -> Result<String, String> {
        let system_prompt = format!(
            "You are a helpful assistant that answers questions about an \
             audio conversation and its knowledge graph. Use the following context from \
             the knowledge graph and recent transcript to answer questions.\n\n\
             Knowledge Graph Context:\n{}",
            graph_context
        );

        let mut text_messages =
            TextMessages::new().add_message(TextMessageRole::System, &system_prompt);

        for msg in messages {
            let role = match msg.role.as_str() {
                "user" => TextMessageRole::User,
                "assistant" => TextMessageRole::Assistant,
                "system" => TextMessageRole::System,
                _ => TextMessageRole::User,
            };
            text_messages = text_messages.add_message(role, &msg.content);
        }

        let response = self
            .rt
            .block_on(self.model.send_chat_request(text_messages))
            .map_err(|e| format!("mistral.rs chat request failed: {}", e))?;

        response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| "No response content from mistral.rs".to_string())
    }

    /// Chat with full message history and knowledge graph context.
    pub fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        graph_context: &str,
    ) -> Result<String, String> {
        self.chat(messages, graph_context)
    }
}
