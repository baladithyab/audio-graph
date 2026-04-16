//! AudioGraph — Real-time audio capture → transcription → knowledge graph
//!
//! This is the Tauri backend for the AudioGraph application.
//! Module structure:
//!   state       — AppState definition (Arc<Mutex<...>>)
//!   commands    — Tauri IPC command handlers
//!   events      — Event name constants and payload types
//!   audio       — Audio capture manager + processing pipeline
//!   asr         — Automatic speech recognition (whisper-rs)
//!   diarization — Speaker diarization (pyannote-rs)
//!   graph       — Temporal knowledge graph (petgraph)
//!   models      — Model management and downloading
//!   persistence — File-based persistence (transcripts + knowledge graph)

pub mod asr;
pub mod audio;
pub mod commands;
pub mod credentials;
pub mod diarization;
pub mod events;
pub mod gemini;
pub mod graph;
pub mod llm;
pub mod models;
pub mod persistence;
pub mod settings;
pub mod speech;
pub mod state;

use state::AppState;

/// Initialize and run the Tauri application.
pub fn run() {
    env_logger::init();

    let app_state = AppState::new();

    // Spawn graph auto-save background thread (saves every 30s).
    {
        let handle = persistence::spawn_graph_autosave(
            &app_state.session_id,
            app_state.knowledge_graph.clone(),
        );
        if let Ok(mut guard) = app_state.graph_autosave_thread.lock() {
            *guard = handle;
        }
    }

    tauri::Builder::default()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::list_audio_sources,
            commands::start_capture,
            commands::stop_capture,
            commands::start_transcribe,
            commands::stop_transcribe,
            commands::get_graph_snapshot,
            commands::get_transcript,
            commands::get_pipeline_status,
            commands::send_chat_message,
            commands::get_chat_history,
            commands::clear_chat_history,
            commands::list_available_models,
            commands::download_model_cmd,
            commands::get_model_status,
            commands::load_llm_model,
            commands::configure_api_endpoint,
            commands::load_settings_cmd,
            commands::save_settings_cmd,
            commands::delete_model_cmd,
            commands::list_running_processes,
            commands::start_gemini,
            commands::stop_gemini,
            // Persistence commands
            commands::export_transcript,
            commands::save_graph,
            commands::load_graph,
            commands::export_graph,
            commands::get_session_id,
            // Credential management
            commands::save_credential_cmd,
            commands::load_credential_cmd,
            commands::load_all_credentials_cmd,
            commands::list_aws_profiles,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AudioGraph");
}
