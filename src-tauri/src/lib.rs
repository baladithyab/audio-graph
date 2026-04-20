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
//!   sessions    — Session metadata index (~/.audiograph/sessions.json)

pub mod asr;
pub mod audio;
pub mod aws_util;
pub mod commands;
pub mod crash_handler;
pub mod credentials;
pub mod diarization;
pub mod error;
pub mod events;
pub mod fs_util;
pub mod gemini;
pub mod graph;
pub mod llm;
pub mod logging;
pub mod models;
pub mod persistence;
pub mod sessions;
pub mod settings;
pub mod speech;
pub mod state;

use state::AppState;
use tauri::Manager;

/// Initialize and run the Tauri application.
pub fn run() {
    // Install the global panic hook before anything else so panics during
    // Tauri startup (builder, state init, plugin load) get captured too.
    crash_handler::install();

    env_logger::init();

    let app_state = AppState::new();
    let initial_session_id = app_state.current_session_id();

    // Register this session in the sessions index (~/.audiograph/sessions.json).
    // Also marks any prior "active" sessions as "crashed" so the UI can
    // distinguish clean shutdowns from crashes.
    if let Err(e) = sessions::register_session(&initial_session_id) {
        log::warn!("Failed to register session in index: {}", e);
    }

    // Surface any persisted token usage from the most-recent prior session
    // so operators can confirm persistence survived the restart. The
    // frontend will wire this to the UI in a later loop; for now it's a
    // log breadcrumb + the `get_session_usage` command is registered below.
    {
        let prior = sessions::load_index();
        if let Some(most_recent) = prior.iter().find(|s| s.id != initial_session_id) {
            let usage = sessions::usage::load_usage(&most_recent.id);
            log::info!(
                "Session restored from prior run {}: {} turns, {} total tokens",
                most_recent.id,
                usage.turns,
                usage.total
            );
        }
    }

    // Spawn graph auto-save background thread (saves every 30s, also refreshes
    // session index stats: segment/speaker/entity counts). The thread reads
    // the current session_id via the shared Arc<RwLock<String>> on each tick
    // so in-process rotation via `new_session_cmd` takes effect without a
    // respawn.
    {
        let handle = persistence::spawn_graph_autosave(
            app_state.session_id.clone(),
            app_state.knowledge_graph.clone(),
            app_state.transcript_buffer.clone(),
        );
        if let Ok(mut guard) = app_state.graph_autosave_thread.lock() {
            *guard = handle;
        }
    }

    // Capture the session_id handle for the shutdown finalizer. At Exit,
    // we read the CURRENT session (may differ from `initial_session_id` if
    // the user rotated via `new_session_cmd`).
    let session_id_handle = app_state.session_id.clone();

    tauri::Builder::default()
        .manage(app_state)
        .setup(|app| {
            // Load the persisted log-level preference as soon as we have an
            // AppHandle (env_logger::init() already ran — this only nudges
            // log::max_level()). RUST_LOG still wins at startup since
            // env_logger honoured it before we got here; the setting only
            // overrides the compiled-in default (Info) and is the level
            // every subsequent `set_log_level` command will persist to.
            let handle = app.handle();
            let settings = crate::settings::load_settings(handle);
            if let Some(ref lvl) = settings.log_level {
                crate::logging::apply_log_level(lvl);
            }
            // Sync the loaded settings into the in-memory cache so other
            // backend modules see them without re-reading the file.
            if let Some(state) = handle.try_state::<AppState>() {
                if let Ok(mut cached) = state.app_settings.write() {
                    *cached = settings;
                }
            }
            Ok(())
        })
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
            commands::set_log_level,
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
            // Session management
            commands::list_sessions,
            commands::load_session_transcript,
            commands::delete_session,
            commands::get_session_usage,
            commands::get_current_session_usage,
            commands::get_lifetime_usage,
            commands::new_session_cmd,
            // Credential management
            commands::save_credential_cmd,
            commands::load_credential_cmd,
            commands::delete_credential_cmd,
            commands::load_all_credentials_cmd,
            commands::diagnose_credentials,
            commands::list_aws_profiles,
            // Cloud provider connection tests
            commands::test_cloud_asr_connection,
            commands::test_deepgram_connection,
            commands::test_assemblyai_connection,
            commands::test_gemini_api_key,
            commands::test_aws_credentials,
        ])
        .build(tauri::generate_context!())
        .expect("error while building AudioGraph")
        .run(move |_app_handle, event| {
            // Mark the session as complete on clean shutdown. Best-effort: if
            // the process is killed we rely on register_session()'s
            // "crashed" detection on the next launch.
            if let tauri::RunEvent::Exit = event {
                let current_sid = match session_id_handle.read() {
                    Ok(g) => g.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                if let Err(e) = crate::sessions::finalize_session(&current_sid) {
                    log::warn!("Failed to finalize session {}: {}", current_sid, e);
                } else {
                    log::info!("Session {} finalized on exit", current_sid);
                }
            }
        });
}
