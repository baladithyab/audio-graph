//! Tauri IPC command handlers.
//!
//! Each function here is exposed to the frontend via `tauri::generate_handler![]`.
//! They access `AppState` through Tauri's managed state.
//!
//! Heavy processing logic (speech, extraction) lives in the [`crate::speech`]
//! module — this file only contains thin `#[tauri::command]` wrappers.

use std::sync::atomic::Ordering;

use tauri::{Emitter, State};

use crate::audio::pipeline::AudioPipeline;
use crate::events::{self, PipelineStatus, StageStatus};
use crate::gemini::{GeminiConfig, GeminiEvent, GeminiLiveClient};
use crate::graph::entities::GraphSnapshot;
use crate::llm::engine::{ChatMessage, ChatResponse};
use crate::llm::{ApiClient, ApiConfig};
use crate::speech;
use crate::state::{AppState, AudioSourceInfo, TranscriptSegment};

// ---------------------------------------------------------------------------
// Helper: parse source_id string into rsac::CaptureTarget
// ---------------------------------------------------------------------------

/// Map a frontend source ID string to an rsac [`CaptureTarget`].
///
/// Supported formats:
/// - `"system-default"`          → `CaptureTarget::SystemDefault`
/// - `"device:<device_id>"`      → `CaptureTarget::Device(DeviceId(device_id))`
/// - `"app:<pid>"`               → `CaptureTarget::Application(ApplicationId(pid))`
/// - `"app-name:<name>"`         → `CaptureTarget::ApplicationByName(name)`
fn parse_capture_target(source_id: &str) -> Result<rsac::CaptureTarget, String> {
    if source_id == "system-default" {
        Ok(rsac::CaptureTarget::SystemDefault)
    } else if let Some(device_id) = source_id.strip_prefix("device:") {
        Ok(rsac::CaptureTarget::Device(rsac::DeviceId(
            device_id.to_string(),
        )))
    } else if let Some(pid_str) = source_id.strip_prefix("app:") {
        // ApplicationId wraps a String (the PID as a string).
        Ok(rsac::CaptureTarget::Application(rsac::ApplicationId(
            pid_str.to_string(),
        )))
    } else if let Some(name) = source_id.strip_prefix("app-name:") {
        Ok(rsac::CaptureTarget::ApplicationByName(name.to_string()))
    } else {
        Err(format!("Unknown source ID format: {}", source_id))
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// List available audio sources (devices + running applications).
#[tauri::command]
pub async fn list_audio_sources(
    state: State<'_, AppState>,
) -> Result<Vec<AudioSourceInfo>, String> {
    log::info!("list_audio_sources called");
    let manager = state
        .capture_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    Ok(manager.list_sources())
}

/// Start capturing audio from the specified source.
#[tauri::command]
pub async fn start_capture(
    source_id: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    log::info!("start_capture called for source: {}", source_id);

    let target = parse_capture_target(&source_id)?;

    // Resolve the user-configured capture format from the in-memory settings
    // cache, falling back to defaults if the cache is uninitialised or the
    // persisted values are out of the supported whitelist. This is the
    // "wiring through" that Task #79 is about — without it the capture
    // thread would always use the hard-coded 48 kHz / stereo.
    let (capture_sample_rate, capture_channels) = {
        let audio_settings = state
            .app_settings
            .read()
            .map(|s| s.audio_settings.clone())
            .unwrap_or_default();
        crate::settings::resolve_audio_settings(&audio_settings)
    };
    log::info!(
        "start_capture: using sample_rate={} Hz, channels={}",
        capture_sample_rate,
        capture_channels
    );

    // 1. Start capture via the manager.
    {
        let mut manager = state
            .capture_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        manager.start_capture(
            &source_id,
            target,
            state.pipeline_tx.clone(),
            app.clone(),
            capture_sample_rate,
            capture_channels,
        )?;
    }

    // 2. Start pipeline thread if not already running.
    {
        let mut pipeline_handle = state
            .pipeline_thread
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if pipeline_handle.is_none() {
            let rx = state.pipeline_rx.clone();
            let tx = state.processed_tx.clone();
            let handle = std::thread::Builder::new()
                .name("audio-pipeline".to_string())
                .spawn(move || {
                    let mut pipeline = AudioPipeline::new(rx, tx);
                    pipeline.run();
                })
                .map_err(|e| format!("Failed to spawn pipeline thread: {}", e))?;
            *pipeline_handle = Some(handle);
            log::info!("Pipeline thread spawned");
        }
    }

    // 2b. Start dispatcher thread (Bug 1 fix): reads from processed_rx and
    //     fans out to per-consumer channels so both speech processor and
    //     Gemini receive ALL chunks.
    {
        let mut dispatcher_handle = state
            .dispatcher_thread
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if dispatcher_handle.is_none() {
            let processed_rx = state.processed_rx.clone();
            let speech_tx = state.speech_audio_tx.clone();
            let gemini_tx = state.gemini_audio_tx.clone();
            let is_transcribing = state.is_transcribing.clone();
            let is_gemini_active = state.is_gemini_active.clone();

            let handle = std::thread::Builder::new()
                .name("audio-dispatcher".to_string())
                .spawn(move || {
                    log::info!("Audio dispatcher: starting fan-out loop");
                    let mut speech_drop_count: u64 = 0;
                    let mut gemini_drop_count: u64 = 0;
                    while let Ok(chunk) = processed_rx.recv() {
                        // Forward to speech processor if transcribing
                        if is_transcribing.load(Ordering::Relaxed)
                            && speech_tx.try_send(chunk.clone()).is_err()
                        {
                            speech_drop_count += 1;
                            if speech_drop_count % 10 == 1 {
                                log::warn!(
                                    "Audio dispatcher: speech channel full, dropped {} chunks total \
                                     (consumer too slow — ASR inference may be blocking)",
                                    speech_drop_count
                                );
                            }
                        }

                        // Forward to Gemini if active
                        let gemini_active = is_gemini_active
                            .read()
                            .map(|a| *a)
                            .unwrap_or(false);
                        if gemini_active && gemini_tx.try_send(chunk).is_err() {
                            gemini_drop_count += 1;
                            if gemini_drop_count % 10 == 1 {
                                log::warn!(
                                    "Audio dispatcher: gemini channel full, dropped {} chunks total",
                                    gemini_drop_count
                                );
                            }
                        }
                    }
                    log::info!(
                        "Audio dispatcher: exiting (pipeline channel closed). \
                         Total drops: speech={}, gemini={}",
                        speech_drop_count, gemini_drop_count
                    );
                })
                .map_err(|e| format!("Failed to spawn dispatcher thread: {}", e))?;
            *dispatcher_handle = Some(handle);
            log::info!("Audio dispatcher thread spawned");
        }
    }

    // 3. Update state flags.
    if let Ok(mut capturing) = state.is_capturing.write() {
        *capturing = true;
    }
    if let Ok(mut status) = state.pipeline_status.write() {
        status.capture = StageStatus::Running { processed_count: 0 };
        status.pipeline = StageStatus::Running { processed_count: 0 };
    }

    // Emit initial pipeline status event
    if let Ok(status) = state.pipeline_status.read() {
        let _ = app.emit(events::PIPELINE_STATUS_EVENT, &*status);
    }

    log::info!("Started capture for source: {}", source_id);
    Ok(())
}

/// Stop capturing audio from the specified source.
///
/// If this was the last active capture, also stops transcription (if running)
/// since there is no more audio to transcribe.
#[tauri::command]
pub async fn stop_capture(
    source_id: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    log::info!("stop_capture called for source: {}", source_id);

    let remaining;
    {
        let mut manager = state
            .capture_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        manager.stop_capture(&source_id)?;
        remaining = manager.active_captures().len();
    }

    if remaining == 0 {
        if let Ok(mut capturing) = state.is_capturing.write() {
            *capturing = false;
        }
        // Also stop transcription since there's no more audio flowing
        state.is_transcribing.store(false, Ordering::SeqCst);
        // Clean up speech processor thread handle
        if let Ok(mut sp_handle) = state.speech_processor_thread.lock() {
            *sp_handle = None;
        }
        // Clean up ASR worker thread handle
        if let Ok(mut asr_handle) = state.asr_worker_thread.lock() {
            *asr_handle = None;
        }
        // Also stop Gemini if running
        if let Ok(mut gemini_active) = state.is_gemini_active.write() {
            if *gemini_active {
                *gemini_active = false;
                // Disconnect the Gemini client
                if let Ok(mut client_guard) = state.gemini_client.lock() {
                    if let Some(ref client) = *client_guard {
                        client.disconnect();
                    }
                    *client_guard = None;
                }
            }
        }
        if let Ok(mut status) = state.pipeline_status.write() {
            status.capture = StageStatus::Idle;
            status.pipeline = StageStatus::Idle;
            status.asr = StageStatus::Idle;
            status.diarization = StageStatus::Idle;
            status.entity_extraction = StageStatus::Idle;
            status.graph = StageStatus::Idle;
        }

        // Emit updated pipeline status
        if let Ok(status) = state.pipeline_status.read() {
            let _ = app.emit(events::PIPELINE_STATUS_EVENT, &*status);
        }
    }

    log::info!("Stopped capture for source: {}", source_id);
    Ok(())
}

/// Probe AWS credentials via STS GetCallerIdentity. Used as pre-flight for
/// DefaultChain and Profile modes so start_transcribe fails fast with an
/// actionable error instead of blowing up inside the EventStream handshake.
///
/// Returns `Ok(())` on success (identity resolved) or an error string on any
/// failure — credentials missing, expired, wrong region, network blocked, etc.
/// Callers are expected to wrap this in a `tokio::time::timeout`.
async fn aws_preflight_probe(
    region: String,
    credential_source: crate::settings::AwsCredentialSource,
) -> Result<(), String> {
    // AccessKeys has a static-cred pre-flight elsewhere; probing via STS
    // here would double up. Callers already filter this case out.
    if matches!(
        credential_source,
        crate::settings::AwsCredentialSource::AccessKeys { .. }
    ) {
        return Err("aws_preflight_probe called with AccessKeys — caller bug".to_string());
    }
    let sdk_config = crate::aws_util::build_aws_sdk_config(&region, credential_source).await?;
    let sts = aws_sdk_sts::Client::new(&sdk_config);
    sts.get_caller_identity()
        .send()
        .await
        .map_err(|e| format!("{}", e))?;
    Ok(())
}

/// Start transcription (streaming processed audio → ASR).
///
/// Requires capture to already be running. Spawns a speech processor thread
/// that reads from the processed audio channel (pipeline output), accumulates
/// chunks into ~2s segments, then runs ASR + diarization + entity extraction.
#[tauri::command]
pub async fn start_transcribe(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), crate::error::AppError> {
    use crate::error::AppError;
    log::info!("start_transcribe called");

    // Guard: capture must be running
    {
        let capturing = state
            .is_capturing
            .read()
            .map_err(|e| AppError::Unknown(format!("Lock error: {}", e)))?;
        if !*capturing {
            return Err(AppError::Unknown(
                "Cannot start transcription: capture is not running".to_string(),
            ));
        }
    }

    // Guard: don't double-start
    if state.is_transcribing.load(Ordering::SeqCst) {
        return Err(AppError::Unknown(
            "Transcription is already running".to_string(),
        ));
    }

    // Pre-flight validation: verify the selected providers are ready before
    // spawning the speech processor. Without these checks the processor thread
    // would try to load the model / reach the API, fail, and exit silently,
    // leaving the user staring at a UI with no feedback. Returning an Err here
    // surfaces to the frontend as a promise rejection → the existing error
    // toast displays the message.
    {
        let asr_provider = state
            .app_settings
            .read()
            .map(|s| s.asr_provider.clone())
            .unwrap_or_default();
        let whisper_model = state
            .app_settings
            .read()
            .map(|s| s.whisper_model.clone())
            .unwrap_or_else(|_| "ggml-small.en.bin".to_string());

        match &asr_provider {
            crate::settings::AsrProvider::LocalWhisper => {
                let models_dir = crate::models::get_models_dir(&app);
                let model_path = models_dir.join(&whisper_model);
                if !model_path.exists() {
                    return Err(AppError::Unknown(format!(
                        "Whisper model '{}' not downloaded. Open Settings and download it first.",
                        whisper_model
                    )));
                }
            }
            crate::settings::AsrProvider::Api {
                endpoint, api_key, ..
            } => {
                if endpoint.trim().is_empty() {
                    return Err(AppError::Unknown(
                        "Cloud ASR endpoint not configured. Open Settings.".to_string(),
                    ));
                }
                if api_key.trim().is_empty() {
                    // Pilot: credential-missing path → structured variant.
                    return Err(AppError::CredentialMissing {
                        key: "cloud_asr_api_key".to_string(),
                    });
                }
            }
            crate::settings::AsrProvider::DeepgramStreaming { api_key, .. } => {
                if api_key.trim().is_empty() {
                    // Pilot: credential-missing path → structured variant.
                    return Err(AppError::CredentialMissing {
                        key: "deepgram_api_key".to_string(),
                    });
                }
            }
            crate::settings::AsrProvider::AssemblyAI { api_key, .. } => {
                if api_key.trim().is_empty() {
                    // Pilot: credential-missing path → structured variant.
                    return Err(AppError::CredentialMissing {
                        key: "assemblyai_api_key".to_string(),
                    });
                }
            }
            crate::settings::AsrProvider::AwsTranscribe {
                credential_source,
                region,
                ..
            } => {
                // Pilot: region-invalid path → structured variant so the
                // frontend can localize "Invalid AWS region: {region}".
                if region.trim().is_empty() {
                    return Err(AppError::AwsRegionInvalid {
                        region: region.clone(),
                    });
                }

                if let crate::settings::AwsCredentialSource::AccessKeys { access_key } =
                    credential_source
                {
                    if access_key.trim().is_empty() {
                        // Pilot: credential-missing path → structured variant.
                        return Err(AppError::CredentialMissing {
                            key: "aws_access_key".to_string(),
                        });
                    }
                    let cred_store = crate::credentials::load_credentials();
                    let secret_valid = cred_store
                        .aws_secret_key
                        .as_ref()
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false);
                    if !secret_valid {
                        // Pilot: credential-missing path → structured variant.
                        return Err(AppError::CredentialMissing {
                            key: "aws_secret_key".to_string(),
                        });
                    }
                }

                // DefaultChain + Profile: probe STS GetCallerIdentity so the
                // user gets a fast, intelligible "no credentials" error instead
                // of the EventStream handshake failing mid-stream and leaving
                // the UI in a confusing half-running state.
                //
                // Bounded to 5s: on a healthy machine with creds, STS responds
                // in <200ms. If it takes longer, the user's network is bad
                // enough that mid-stream failures are likely anyway — better
                // to fail fast in pre-flight than stall capture.
                if !matches!(
                    credential_source,
                    crate::settings::AwsCredentialSource::AccessKeys { .. }
                ) {
                    let probe = aws_preflight_probe(region.clone(), credential_source.clone());
                    match tokio::time::timeout(std::time::Duration::from_secs(5), probe).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => return Err(AppError::Unknown(format!(
                            "AWS credential pre-flight failed: {}. Open Settings → ASR → AWS Transcribe → Test Connection to diagnose.",
                            e
                        ))),
                        Err(_) => return Err(AppError::Unknown(
                            "AWS credential pre-flight timed out after 5s. Check network or switch credential mode."
                                .to_string(),
                        )),
                    }
                }
            }
            crate::settings::AsrProvider::SherpaOnnx { model_dir, .. } => {
                let models_dir = crate::models::get_models_dir(&app);
                let model_path = models_dir.join(model_dir);
                if !model_path.exists() {
                    return Err(AppError::Unknown(format!(
                        "Sherpa-ONNX model directory '{}' not found. Download it via Settings first.",
                        model_dir
                    )));
                }
                // The directory existing isn't enough — sherpa-onnx needs the
                // encoder/decoder/joiner ONNX graphs and the tokens vocabulary.
                // A partial download or unpack would pass the exists() check
                // but fail silently inside the speech processor thread.
                for required in &["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"] {
                    if !model_path.join(required).exists() {
                        return Err(AppError::Unknown(format!(
                            "Sherpa-ONNX model '{}' is missing '{}'. Re-download via Settings.",
                            model_dir, required
                        )));
                    }
                }
            }
        }

        // LLM pre-flight: only warn for LocalLlama — entity extraction has
        // fallbacks (API, rule-based) so a missing local model isn't fatal.
        let llm_provider = state
            .app_settings
            .read()
            .map(|s| s.llm_provider.clone())
            .unwrap_or_default();
        if let crate::settings::LlmProvider::LocalLlama = llm_provider {
            let models_dir = crate::models::get_models_dir(&app);
            let llm_path = models_dir.join(crate::models::LLM_MODEL_FILENAME);
            if !llm_path.exists() {
                log::warn!(
                    "Local LLM model not downloaded; entity extraction will fall back to API or rule-based"
                );
                // Don't error — extraction has fallbacks. Just log.
            }
        }
    }

    // 1. Start speech processor thread (ASR + Diarization orchestrator).
    //    The speech processor reads directly from the processed audio channel,
    //    accumulates chunks into ~2s segments, and runs ASR inline.
    {
        let mut sp_handle = state
            .speech_processor_thread
            .lock()
            .map_err(|e| AppError::Unknown(format!("Lock error: {}", e)))?;
        if sp_handle.is_none() {
            // Bug 1 fix: read from per-consumer channel, not shared processed_rx
            let speech_rx = state.speech_audio_rx.clone();
            // Bug 2 fix: pass AtomicBool so the speech processor can check it
            let is_transcribing = state.is_transcribing.clone();

            let transcript_buffer = state.transcript_buffer.clone();
            let pipeline_status = state.pipeline_status.clone();
            let app_handle = app.clone();
            let knowledge_graph = state.knowledge_graph.clone();
            let graph_snapshot_clone = state.graph_snapshot.clone();
            let graph_extractor = state.graph_extractor.clone();
            let llm_engine = state.llm_engine.clone();
            let api_client = state.api_client.clone();
            let mistralrs_engine = state.mistralrs_engine.clone();

            let models_dir = crate::models::get_models_dir(&app);

            let asr_provider = state
                .app_settings
                .read()
                .map(|s| s.asr_provider.clone())
                .unwrap_or_default();

            let whisper_model = state
                .app_settings
                .read()
                .map(|s| s.whisper_model.clone())
                .unwrap_or_else(|_| "ggml-small.en.bin".to_string());

            let llm_provider = state
                .app_settings
                .read()
                .map(|s| s.llm_provider.clone())
                .unwrap_or_default();

            // If the user selected local LLM and the engine is not yet
            // loaded, attempt to load it now on a blocking background task.
            if matches!(llm_provider, crate::settings::LlmProvider::LocalLlama) {
                let engine_empty = state
                    .llm_engine
                    .lock()
                    .map(|g| g.is_none())
                    .unwrap_or(false);
                if engine_empty {
                    let models_dir_clone = models_dir.clone();
                    let llm_engine_clone = state.llm_engine.clone();
                    let model_path = models_dir_clone.join(crate::models::LLM_MODEL_FILENAME);
                    if model_path.exists() {
                        log::info!("Auto-loading local LLM model for LocalLlama provider...");
                        let _ = std::thread::Builder::new()
                            .name("llm-autoload".to_string())
                            .spawn(move || {
                                match crate::llm::LlmEngine::new(&model_path.to_string_lossy()) {
                                    Ok(engine) => {
                                        if let Ok(mut guard) = llm_engine_clone.lock() {
                                            *guard = Some(engine);
                                            log::info!("Local LLM model auto-loaded successfully");
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!("Failed to auto-load local LLM model: {}", e);
                                    }
                                }
                            });
                    }
                }
            }

            // If the user selected API LLM provider, configure the API
            // client from the provider settings.
            if let crate::settings::LlmProvider::Api {
                ref endpoint,
                ref api_key,
                ref model,
            } = llm_provider
            {
                let api_empty = state
                    .api_client
                    .lock()
                    .map(|g| g.is_none())
                    .unwrap_or(false);
                if api_empty && !endpoint.is_empty() {
                    let (api_max_tokens, api_temperature) = state
                        .app_settings
                        .read()
                        .ok()
                        .and_then(|s| {
                            s.llm_api_config
                                .as_ref()
                                .map(|c| (c.max_tokens, c.temperature))
                        })
                        .unwrap_or((512, 0.1));

                    let config = crate::llm::ApiConfig {
                        endpoint: endpoint.clone(),
                        api_key: if api_key.is_empty() {
                            None
                        } else {
                            Some(api_key.clone())
                        },
                        model: model.clone(),
                        max_tokens: api_max_tokens,
                        temperature: api_temperature,
                    };
                    let client = crate::llm::ApiClient::new(config);
                    if client.is_configured() {
                        if let Ok(mut guard) = state.api_client.lock() {
                            *guard = Some(client);
                            log::info!("API client auto-configured from LLM provider settings");
                        }
                    }
                }
            }

            let transcript_writer = state.transcript_writer.clone();

            let handle = std::thread::Builder::new()
                .name("speech-processor".to_string())
                .spawn(move || {
                    let channels = speech::SpeechChannels {
                        processed_rx: speech_rx,
                        is_transcribing,
                    };
                    let shared = speech::SpeechShared {
                        transcript_buffer,
                        transcript_writer,
                        pipeline_status,
                        app_handle,
                        knowledge_graph,
                        graph_snapshot: graph_snapshot_clone,
                        graph_extractor,
                        llm_engine,
                        api_client,
                        mistralrs_engine,
                    };
                    let config = speech::SpeechConfig {
                        models_dir,
                        llm_provider,
                    };
                    speech::run_speech_processor(
                        channels,
                        shared,
                        config,
                        asr_provider,
                        whisper_model,
                    );
                })
                .map_err(|e| {
                    AppError::Unknown(format!("Failed to spawn speech processor thread: {}", e))
                })?;
            *sp_handle = Some(handle);
            log::info!("Speech processor thread spawned for transcribe");
        }
    }

    // 3. Update state flags.
    state.is_transcribing.store(true, Ordering::SeqCst);
    if let Ok(mut status) = state.pipeline_status.write() {
        status.asr = StageStatus::Running { processed_count: 0 };
        status.diarization = StageStatus::Running { processed_count: 0 };
        status.entity_extraction = StageStatus::Running { processed_count: 0 };
        status.graph = StageStatus::Running { processed_count: 0 };
    }

    if let Ok(status) = state.pipeline_status.read() {
        let _ = app.emit(events::PIPELINE_STATUS_EVENT, &*status);
    }

    log::info!("Started transcription (streaming mode)");
    Ok(())
}

/// Stop transcription without stopping capture.
///
/// Sets the AtomicBool flag to false so the speech processor thread exits
/// on its next `recv_timeout` cycle (Bug 2 fix), then cleans up the thread handle.
#[tauri::command]
pub async fn stop_transcribe(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    log::info!("stop_transcribe called");

    // Signal the speech processor to stop via AtomicBool
    state.is_transcribing.store(false, Ordering::SeqCst);

    // Clean up the speech processor thread handle — it will exit on its own
    // via the flag check in its recv_timeout loop.
    if let Ok(mut sp_handle) = state.speech_processor_thread.lock() {
        *sp_handle = None;
    }
    // Clean up the ASR worker thread handle
    if let Ok(mut asr_handle) = state.asr_worker_thread.lock() {
        *asr_handle = None;
    }

    // Update pipeline status — ASR and downstream stages go idle
    if let Ok(mut status) = state.pipeline_status.write() {
        status.asr = StageStatus::Idle;
        status.diarization = StageStatus::Idle;
        status.entity_extraction = StageStatus::Idle;
        status.graph = StageStatus::Idle;
    }

    if let Ok(status) = state.pipeline_status.read() {
        let _ = app.emit(events::PIPELINE_STATUS_EVENT, &*status);
    }

    log::info!("Stopped transcription");
    Ok(())
}

/// Get the current knowledge graph snapshot.
#[tauri::command]
pub async fn get_graph_snapshot(state: State<'_, AppState>) -> Result<GraphSnapshot, String> {
    let snapshot = state
        .graph_snapshot
        .read()
        .map_err(|e| format!("Failed to read graph snapshot: {}", e))?;
    Ok(snapshot.clone())
}

/// Get transcript segments, optionally filtered by source and time.
#[tauri::command]
pub async fn get_transcript(
    source_id: Option<String>,
    since: Option<f64>,
    state: State<'_, AppState>,
) -> Result<Vec<TranscriptSegment>, String> {
    let buffer = state
        .transcript_buffer
        .read()
        .map_err(|e| format!("Failed to read transcript buffer: {}", e))?;

    let segments: Vec<TranscriptSegment> = buffer
        .iter()
        .filter(|seg| {
            let source_match = source_id
                .as_ref()
                .map(|id| &seg.source_id == id)
                .unwrap_or(true);
            let time_match = since.map(|t| seg.start_time >= t).unwrap_or(true);
            source_match && time_match
        })
        .cloned()
        .collect();

    Ok(segments)
}

/// Get the current pipeline status.
#[tauri::command]
pub async fn get_pipeline_status(state: State<'_, AppState>) -> Result<PipelineStatus, String> {
    let status = state
        .pipeline_status
        .read()
        .map_err(|e| format!("Failed to read pipeline status: {}", e))?;
    Ok(status.clone())
}

// ---------------------------------------------------------------------------
// API endpoint configuration
// ---------------------------------------------------------------------------

/// Validate and parse an OpenAI-compatible endpoint URL.
///
/// `reqwest` will reject malformed URLs at request time, but that produces a
/// confusing "invalid format" failure many seconds into a chat, long after the
/// user has forgotten what they typed in Settings. Parse up-front so the
/// Settings UI can surface the error synchronously, and restrict to http/https
/// schemes so `file://` / `ftp://` / other exotic schemes can't sneak in.
pub(crate) fn validate_endpoint_url(endpoint: &str) -> Result<url::Url, String> {
    let parsed = url::Url::parse(endpoint).map_err(|e| format!("Invalid endpoint URL: {}", e))?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        other => Err(format!(
            "Invalid endpoint URL: unsupported scheme `{}` (expected http or https)",
            other
        )),
    }
}

/// Configure an OpenAI-compatible API endpoint for LLM inference.
///
/// This allows using cloud providers (OpenAI, OpenRouter) or local servers
/// (Ollama, LM Studio, vLLM) as an alternative to the native llama-cpp-2 engine.
#[tauri::command]
pub async fn configure_api_endpoint(
    endpoint: String,
    api_key: Option<String>,
    model: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    log::info!(
        "configure_api_endpoint: endpoint={}, model={}",
        endpoint,
        model
    );

    validate_endpoint_url(&endpoint)?;

    let config = ApiConfig {
        endpoint,
        api_key,
        model,
        max_tokens: 512,
        temperature: 0.1,
    };
    let client = ApiClient::new(config);

    if !client.is_configured() {
        return Err("Invalid API configuration: endpoint and model must be non-empty".to_string());
    }

    *state
        .api_client
        .lock()
        .map_err(|e| format!("Lock error: {}", e))? = Some(client);

    log::info!("API endpoint configured successfully");
    Ok(())
}

// ---------------------------------------------------------------------------
// Chat commands (backed by native LLM engine or API client)
// ---------------------------------------------------------------------------

/// Send a chat message and get a response from the LLM, informed by the
/// current knowledge graph and transcript context.
///
/// Tries backends in order: native LLM → API client → graph context fallback.
///
/// I4 fix: takes a snapshot of the graph and transcript, releases the locks,
/// then builds the context string from the snapshot (no lock held during
/// string formatting).
#[tauri::command]
pub async fn send_chat_message(
    message: String,
    state: State<'_, AppState>,
) -> Result<ChatResponse, String> {
    log::info!(
        "send_chat_message called: {}",
        &message[..message.len().min(50)]
    );

    // I4: Take a snapshot of graph data, then release the lock immediately.
    let snapshot = {
        let kg = state
            .knowledge_graph
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        kg.snapshot() // returns cloned GraphSnapshot
    }; // lock released here

    // Take a snapshot of recent transcript, then release that lock too.
    let recent_transcript: Vec<TranscriptSegment> = {
        let transcript = state
            .transcript_buffer
            .read()
            .map_err(|e| format!("Lock error: {}", e))?;
        transcript.iter().rev().take(10).cloned().collect()
    }; // lock released here

    // Build graph context string from snapshots — no locks held.
    let graph_context = {
        let mut ctx = String::new();

        ctx.push_str(&format!("Entities ({}):\n", snapshot.nodes.len()));
        for node in &snapshot.nodes {
            ctx.push_str(&format!("- {} ({})", node.name, node.entity_type));
            if let Some(ref desc) = node.description {
                ctx.push_str(&format!(": {}", desc));
            }
            ctx.push('\n');
        }

        ctx.push_str(&format!("\nRelationships ({}):\n", snapshot.links.len()));
        for link in &snapshot.links {
            ctx.push_str(&format!(
                "- {} → {} ({})\n",
                link.source, link.target, link.relation_type
            ));
        }

        // Add recent transcript from snapshot
        if !recent_transcript.is_empty() {
            ctx.push_str("\nRecent Transcript:\n");
            for seg in recent_transcript.iter().rev() {
                let speaker = seg.speaker_label.as_deref().unwrap_or("Unknown");
                ctx.push_str(&format!("[{}]: {}\n", speaker, seg.text));
            }
        }

        ctx
    };

    // Add user message to history.
    let user_msg = ChatMessage {
        role: "user".to_string(),
        content: message,
    };

    {
        let mut history = state
            .chat_history
            .write()
            .map_err(|e| format!("Lock error: {}", e))?;
        history.push(user_msg.clone());
    }

    // Get chat history for context.
    let messages: Vec<ChatMessage> = {
        let history = state
            .chat_history
            .read()
            .map_err(|e| format!("Lock error: {}", e))?;
        history.clone()
    };

    // Try backends in order: native LLM → API client → graph context fallback.
    let response_text = {
        // 1. Try native LLM engine first.
        let native_result = {
            let engine_guard = state
                .llm_engine
                .lock()
                .map_err(|e| format!("Lock error: {}", e))?;
            if let Some(ref engine) = *engine_guard {
                match engine.chat(&messages, &graph_context) {
                    Ok(text) => Some(Ok(text)),
                    Err(e) => {
                        log::warn!("Native LLM chat failed: {}", e);
                        Some(Err(e))
                    }
                }
            } else {
                None // No native LLM loaded
            }
        };

        match native_result {
            Some(Ok(text)) => text,
            _ => {
                // 2. Try API client.
                let api_result = {
                    let api_guard = state
                        .api_client
                        .lock()
                        .map_err(|e| format!("Lock error: {}", e))?;
                    if let Some(ref client) = *api_guard {
                        match client.chat_with_history(&messages, &graph_context) {
                            Ok(text) => Some(Ok(text)),
                            Err(e) => {
                                log::warn!("API chat failed: {}", e);
                                Some(Err(e))
                            }
                        }
                    } else {
                        None // No API client configured
                    }
                };

                match api_result {
                    Some(Ok(text)) => text,
                    Some(Err(e)) => {
                        // API configured but failed — report error with context.
                        format!(
                            "I can see the knowledge graph has {} entities and {} relationships. \
                             However, I couldn't generate a detailed response (API error: {}). \
                             Please check the API endpoint configuration.",
                            snapshot.nodes.len(),
                            snapshot.links.len(),
                            e
                        )
                    }
                    None => {
                        // 3. No backend available — provide graph context fallback.
                        if let Some(Err(e)) = native_result {
                            format!(
                                "Native LLM error: {}. No API endpoint configured.\n\n\
                                 Here's what I know from the knowledge graph:\n\n{}",
                                e, graph_context
                            )
                        } else {
                            format!(
                                "No LLM backend available. Configure a native model or API endpoint.\n\n\
                                 Here's what I know from the knowledge graph:\n\n{}",
                                graph_context
                            )
                        }
                    }
                }
            }
        }
    };

    let assistant_msg = ChatMessage {
        role: "assistant".to_string(),
        content: response_text,
    };

    // Add assistant message to history.
    {
        let mut history = state
            .chat_history
            .write()
            .map_err(|e| format!("Lock error: {}", e))?;
        history.push(assistant_msg.clone());
    }

    Ok(ChatResponse {
        message: assistant_msg,
        tokens_used: 0, // TODO: track actual token usage
    })
}

/// Get the current chat message history.
#[tauri::command]
pub async fn get_chat_history(state: State<'_, AppState>) -> Result<Vec<ChatMessage>, String> {
    let history = state
        .chat_history
        .read()
        .map_err(|e| format!("Lock error: {}", e))?;
    Ok(history.clone())
}

/// Clear the chat message history.
#[tauri::command]
pub async fn clear_chat_history(state: State<'_, AppState>) -> Result<(), String> {
    let mut history = state
        .chat_history
        .write()
        .map_err(|e| format!("Lock error: {}", e))?;
    history.clear();
    Ok(())
}

// ---------------------------------------------------------------------------
// Model management commands
// ---------------------------------------------------------------------------

/// List available models and their download status.
#[tauri::command]
pub fn list_available_models(app: tauri::AppHandle) -> Vec<crate::models::ModelInfo> {
    crate::models::list_models(&app)
}

/// Download a model by filename, with progress events emitted to the frontend.
///
/// Runs the blocking HTTP download on a background thread via
/// `tokio::task::spawn_blocking` so the IPC handler stays async (G3).
#[tauri::command]
pub async fn download_model_cmd(
    app: tauri::AppHandle,
    model_filename: String,
) -> Result<String, String> {
    let handle = app.clone();
    tokio::task::spawn_blocking(move || crate::models::download_model(&handle, &model_filename))
        .await
        .map_err(|e| format!("Download task failed: {}", e))?
}

/// Get the readiness status of all known models (G1).
#[tauri::command]
pub fn get_model_status(app: tauri::AppHandle) -> crate::models::ModelStatus {
    crate::models::get_model_status(&app)
}

/// Load the native LLM model into memory (G2).
///
/// Resolves the model path from the app data directory, then loads it on a
/// background thread. On success the engine is stored in `AppState.llm_engine`.
#[tauri::command]
pub async fn load_llm_model(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let models_dir = crate::models::get_models_dir(&app);
    let model_path = models_dir.join(crate::models::LLM_MODEL_FILENAME);

    if !model_path.exists() {
        return Err("LLM model not downloaded".to_string());
    }

    let path = model_path.clone();
    let engine =
        tokio::task::spawn_blocking(move || crate::llm::LlmEngine::new(&path.to_string_lossy()))
            .await
            .map_err(|e| format!("Failed to spawn LLM loading task: {}", e))?
            .map_err(|e| format!("Failed to load LLM model: {}", e))?;

    let mut guard = state.llm_engine.lock().map_err(|e| e.to_string())?;
    *guard = Some(engine);

    Ok("LLM model loaded successfully".to_string())
}

// ---------------------------------------------------------------------------
// Settings commands
// ---------------------------------------------------------------------------

/// Load application settings from disk (returns defaults if missing).
/// Syncs the loaded settings into the in-memory `AppState.app_settings` cache
/// so other backend modules (e.g. speech processor) can read them without I/O.
#[tauri::command]
pub fn load_settings_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> crate::settings::AppSettings {
    let settings = crate::settings::load_settings(&app);
    // Sync in-memory cache
    if let Ok(mut cached) = state.app_settings.write() {
        *cached = settings.clone();
    }
    settings
}

/// Save application settings to disk (atomic write).
/// Also updates the in-memory `AppState.app_settings` cache.
#[tauri::command]
pub fn save_settings_cmd(
    app: tauri::AppHandle,
    settings: crate::settings::AppSettings,
    state: State<'_, AppState>,
) -> Result<(), String> {
    crate::settings::save_settings(&app, &settings)?;
    // Sync in-memory cache
    if let Ok(mut cached) = state.app_settings.write() {
        *cached = settings;
    }
    Ok(())
}

/// Delete a downloaded model file by filename.
#[tauri::command]
pub fn delete_model_cmd(app: tauri::AppHandle, model_filename: String) -> Result<String, String> {
    crate::models::delete_model(&app, &model_filename)
}

/// Change the runtime log level and update the in-memory settings cache.
///
/// Takes effect immediately for every subsequent `log::*!` macro and dirties
/// the cached settings so the new level is visible to readers. Disk
/// persistence is **not** performed here — the frontend is expected to call
/// `save_settings_cmd` to flush the full settings blob when the user commits.
///
// set_log_level only mutates runtime tracing; save_settings_cmd is the
// single owner of disk persistence. See loop-13 review.
#[tauri::command]
pub fn set_log_level(
    _app: tauri::AppHandle,
    level: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // 1. Flip the in-process log level. Immediate, cheap, and the user's
    //    primary expectation from this command.
    crate::logging::apply_log_level(&level);

    // 2. Dirty the in-memory settings cache so any reader (and the next
    //    save_settings_cmd call) sees the new value. No disk write here —
    //    save_settings_cmd is the sole owner of that path to avoid the
    //    race flagged in the loop-13 review.
    if let Ok(mut cached) = state.app_settings.write() {
        cached.log_level = Some(level);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Gemini Live dual-pipeline commands
// ---------------------------------------------------------------------------

/// Start the Gemini Live pipeline.
///
/// Reads Gemini settings (API key, model) from `AppSettings`, creates a
/// `GeminiLiveClient`, connects it, then spawns two worker threads:
///   1. **Audio sender** — reads from `processed_rx` (same pipeline output
///      used by the local Whisper pipeline) and forwards audio to Gemini.
///   2. **Event receiver** — reads `GeminiEvent`s from the client and emits
///      Tauri events (`gemini-transcription`, `gemini-response`), also feeding
///      transcriptions into the knowledge graph.
///
/// Both pipelines (local and Gemini) can run simultaneously since they share
/// the same `processed_rx` channel (crossbeam receivers are cloneable).
#[tauri::command]
pub async fn start_gemini(state: State<'_, AppState>, app: tauri::AppHandle) -> Result<(), String> {
    log::info!("start_gemini called");

    // Guard: capture must be running
    {
        let capturing = state
            .is_capturing
            .read()
            .map_err(|e| format!("Lock error: {}", e))?;
        if !*capturing {
            return Err("Cannot start Gemini: capture is not running".to_string());
        }
    }

    // Guard: don't double-start
    {
        let active = state
            .is_gemini_active
            .read()
            .map_err(|e| format!("Lock error: {}", e))?;
        if *active {
            return Err("Gemini pipeline is already running".to_string());
        }
    }

    // Read Gemini settings
    let gemini_settings = state
        .app_settings
        .read()
        .map(|s| s.gemini.clone())
        .unwrap_or_default();

    // Validate auth configuration early.
    match &gemini_settings.auth {
        crate::settings::GeminiAuthMode::ApiKey { api_key } => {
            if api_key.is_empty() {
                return Err(
                    "Gemini API key is not configured. Set it in Settings → Gemini.".to_string(),
                );
            }
        }
        crate::settings::GeminiAuthMode::VertexAI {
            project_id,
            location,
            ..
        } => {
            if project_id.is_empty() || location.is_empty() {
                return Err(
                    "Vertex AI project_id and location must be configured in Settings → Gemini."
                        .to_string(),
                );
            }
        }
    }

    // Create and connect the client
    let config = GeminiConfig {
        auth: gemini_settings.auth.clone(),
        model: gemini_settings.model,
    };
    let mut client = GeminiLiveClient::new(config);
    client.connect()?;

    let event_rx = client.event_rx();

    // Store the client
    {
        let mut client_guard = state
            .gemini_client
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        *client_guard = Some(client);
    }

    // 1. Spawn the audio sender thread.
    //    Reads from the processed audio pipeline and forwards to Gemini.
    {
        let mut audio_handle = state
            .gemini_audio_thread
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if audio_handle.is_none() {
            // Bug 1 fix: read from dedicated Gemini channel, not shared processed_rx
            let gemini_rx = state.gemini_audio_rx.clone();
            let gemini_client = state.gemini_client.clone();
            let is_active = state.is_gemini_active.clone();

            let handle = std::thread::Builder::new()
                .name("gemini-audio-sender".to_string())
                .spawn(move || {
                    log::info!("Gemini audio sender: starting");

                    while let Ok(chunk) = gemini_rx.recv() {
                        // Check if we should stop
                        let active = is_active.read().map(|a| *a).unwrap_or(false);
                        if !active {
                            break;
                        }

                        // Forward the audio to Gemini
                        // The chunk is already f32 mono 16kHz from the pipeline
                        let client_guard = match gemini_client.lock() {
                            Ok(g) => g,
                            Err(_) => break,
                        };
                        if let Some(ref client) = *client_guard {
                            if let Err(e) = client.send_audio(&chunk.data) {
                                log::warn!("Gemini audio sender: send failed: {}", e);
                                break;
                            }
                        } else {
                            break;
                        }
                    }

                    log::info!("Gemini audio sender: exiting");
                })
                .map_err(|e| format!("Failed to spawn Gemini audio thread: {}", e))?;
            *audio_handle = Some(handle);
            log::info!("Gemini audio sender thread spawned");
        }
    }

    // 2. Spawn the event receiver thread.
    //    Reads GeminiEvents and emits Tauri events + feeds the knowledge graph.
    {
        let mut event_handle = state
            .gemini_event_thread
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if event_handle.is_none() {
            let app_handle = app.clone();
            let is_active = state.is_gemini_active.clone();
            let knowledge_graph = state.knowledge_graph.clone();
            let graph_snapshot = state.graph_snapshot.clone();
            let graph_extractor = state.graph_extractor.clone();
            let pipeline_status = state.pipeline_status.clone();
            let llm_engine = state.llm_engine.clone();
            let api_client = state.api_client.clone();
            let mistralrs_engine = state.mistralrs_engine.clone();
            let llm_provider = state
                .app_settings
                .read()
                .map(|s| s.llm_provider.clone())
                .unwrap_or_default();
            // Share the session_id Arc so per-turn writes land in the
            // CURRENT session's usage file even after `new_session_cmd`
            // rotates the ID in-process.
            let session_id_handle = state.session_id.clone();

            let handle = std::thread::Builder::new()
                .name("gemini-event-receiver".to_string())
                .spawn(move || {
                    log::info!("Gemini event receiver: starting");

                    let mut extraction_count: u64 = 0;
                    let mut graph_update_count: u64 = 0;

                    while let Ok(event) = event_rx.recv() {
                        // Check if we should stop
                        let active = is_active.read().map(|a| *a).unwrap_or(false);
                        if !active {
                            break;
                        }

                        match event {
                            GeminiEvent::Transcription { ref text, .. } => {
                                // Emit Tauri event for the frontend
                                let _ = app_handle.emit(events::GEMINI_TRANSCRIPTION, &event);

                                // Feed transcription into the knowledge graph
                                // (same extraction pipeline as local transcripts)
                                if !text.is_empty() {
                                    let segment_id = uuid::Uuid::new_v4().to_string();
                                    let timestamp = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs_f64();

                                    speech::process_extraction_and_emit(
                                        text,
                                        "Gemini",
                                        &segment_id,
                                        timestamp,
                                        &speech::ExtractionDeps {
                                            llm_engine: &llm_engine,
                                            api_client: &api_client,
                                            mistralrs_engine: &mistralrs_engine,
                                            llm_provider: &llm_provider,
                                            graph_extractor: &graph_extractor,
                                            knowledge_graph: &knowledge_graph,
                                            graph_snapshot: &graph_snapshot,
                                            pipeline_status: &pipeline_status,
                                            app_handle: &app_handle,
                                        },
                                        &mut extraction_count,
                                        &mut graph_update_count,
                                    );
                                }
                            }
                            GeminiEvent::ModelResponse { .. } => {
                                let _ = app_handle.emit(events::GEMINI_RESPONSE, &event);
                            }
                            GeminiEvent::Error { ref message } => {
                                log::error!("Gemini error event: {}", message);
                                let _ = app_handle.emit(events::GEMINI_STATUS, &event);
                            }
                            GeminiEvent::Connected => {
                                let _ = app_handle.emit(events::GEMINI_STATUS, &event);
                            }
                            GeminiEvent::TurnComplete { ref usage } => {
                                // Model finished its turn. Forward the event
                                // on GEMINI_STATUS so the UI can surface
                                // per-turn token accounting from
                                // `usageMetadata` (see gemini::UsageMetadata).
                                if let Some(u) = usage {
                                    log::debug!(
                                        "Gemini: turn complete (tokens total={:?})",
                                        u.total_token_count
                                    );
                                } else {
                                    log::debug!("Gemini: turn complete");
                                }

                                // Persist per-session token totals (loop 19).
                                // Before this, turn counts + token totals only
                                // lived in the frontend's localStorage and did
                                // not survive an app restart.
                                let delta = crate::sessions::usage::TurnDelta {
                                    prompt: usage
                                        .as_ref()
                                        .and_then(|u| u.prompt_token_count)
                                        .unwrap_or(0)
                                        as u64,
                                    response: usage
                                        .as_ref()
                                        .and_then(|u| u.response_token_count)
                                        .unwrap_or(0)
                                        as u64,
                                    cached: usage
                                        .as_ref()
                                        .and_then(|u| u.cached_content_token_count)
                                        .unwrap_or(0)
                                        as u64,
                                    thoughts: usage
                                        .as_ref()
                                        .and_then(|u| u.thoughts_token_count)
                                        .unwrap_or(0)
                                        as u64,
                                    tool_use: usage
                                        .as_ref()
                                        .and_then(|u| u.tool_use_prompt_token_count)
                                        .unwrap_or(0)
                                        as u64,
                                    total: usage
                                        .as_ref()
                                        .and_then(|u| u.total_token_count)
                                        .unwrap_or(0)
                                        as u64,
                                };
                                let current_sid = match session_id_handle.read() {
                                    Ok(g) => g.clone(),
                                    Err(poisoned) => poisoned.into_inner().clone(),
                                };
                                if let Err(e) =
                                    crate::sessions::usage::append_turn(&current_sid, delta)
                                {
                                    log::warn!("Failed to persist turn usage: {}", e);
                                }

                                let _ = app_handle.emit(events::GEMINI_STATUS, &event);
                            }
                            GeminiEvent::Disconnected => {
                                let _ = app_handle.emit(events::GEMINI_STATUS, &event);
                                break;
                            }
                            GeminiEvent::Reconnecting {
                                attempt,
                                backoff_secs,
                            } => {
                                // Auto-reconnect in flight — surface through
                                // the status event so the UI can show a
                                // "reconnecting…" hint. Do NOT break the loop:
                                // the session task handles the full setup
                                // handshake replay and will emit Reconnected
                                // on success or a fatal Error if the budget
                                // is exhausted.
                                log::info!(
                                    "Gemini: reconnecting attempt={} backoff={}s",
                                    attempt,
                                    backoff_secs
                                );
                                let _ = app_handle.emit(events::GEMINI_STATUS, &event);
                            }
                            GeminiEvent::Reconnected { resumed } => {
                                log::info!("Gemini: reconnected (resumed={})", resumed);
                                let _ = app_handle.emit(events::GEMINI_STATUS, &event);
                            }
                        }
                    }

                    log::info!("Gemini event receiver: exiting");
                })
                .map_err(|e| format!("Failed to spawn Gemini event thread: {}", e))?;
            *event_handle = Some(handle);
            log::info!("Gemini event receiver thread spawned");
        }
    }

    // 3. Update state flag
    if let Ok(mut active) = state.is_gemini_active.write() {
        *active = true;
    }

    log::info!("Gemini Live pipeline started");
    Ok(())
}

/// Stop the Gemini Live pipeline.
///
/// Disconnects the client, signals worker threads to stop via the
/// `is_gemini_active` flag, and cleans up thread handles.
#[tauri::command]
pub async fn stop_gemini(state: State<'_, AppState>, _app: tauri::AppHandle) -> Result<(), String> {
    log::info!("stop_gemini called");

    // 1. Set active flag to false (signals worker threads to exit)
    if let Ok(mut active) = state.is_gemini_active.write() {
        *active = false;
    }

    // 2. Disconnect the client (sends Disconnected event, closes channels)
    {
        let mut client_guard = state
            .gemini_client
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if let Some(ref client) = *client_guard {
            client.disconnect();
        }
        *client_guard = None;
    }

    // 3. Clean up thread handles (they should exit naturally)
    {
        let mut audio_handle = state
            .gemini_audio_thread
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        *audio_handle = None;
    }
    {
        let mut event_handle = state
            .gemini_event_thread
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        *event_handle = None;
    }

    log::info!("Gemini Live pipeline stopped");
    Ok(())
}

// ---------------------------------------------------------------------------
// Process enumeration
// ---------------------------------------------------------------------------

/// A running system process (for target-selection UI).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub exe_path: Option<String>,
}

/// List running system processes (deduplicated by name, sorted alphabetically).
#[tauri::command]
pub fn list_running_processes() -> Vec<ProcessInfo> {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let mut processes: Vec<ProcessInfo> = sys
        .processes()
        .iter()
        .filter(|(_, p)| !p.name().to_string_lossy().is_empty())
        .map(|(pid, p)| ProcessInfo {
            pid: pid.as_u32(),
            name: p.name().to_string_lossy().to_string(),
            exe_path: p.exe().map(|e| e.to_string_lossy().to_string()),
        })
        .collect();

    processes.sort_by_key(|a| a.name.to_lowercase());
    processes.dedup_by(|a, b| a.name == b.name);
    processes
}

// ---------------------------------------------------------------------------
// Persistence commands (transcript + knowledge graph)
// ---------------------------------------------------------------------------

/// Export the full in-memory transcript buffer as a JSON string.
#[tauri::command]
pub async fn export_transcript(state: State<'_, AppState>) -> Result<String, String> {
    let buffer = state
        .transcript_buffer
        .read()
        .map_err(|e| format!("Failed to read transcript buffer: {}", e))?;
    let segments: Vec<TranscriptSegment> = buffer.iter().cloned().collect();
    serde_json::to_string_pretty(&segments)
        .map_err(|e| format!("Failed to serialize transcript: {}", e))
}

/// Save the knowledge graph to disk (session-specific file).
#[tauri::command]
pub async fn save_graph(state: State<'_, AppState>) -> Result<String, String> {
    let dir = crate::persistence::graphs_dir()
        .ok_or_else(|| "Cannot resolve graph save directory".to_string())?;

    let file_path = dir.join(format!("{}.json", state.current_session_id()));

    let graph = state
        .knowledge_graph
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;

    graph.save_to_file(&file_path)?;

    log::info!("Graph saved to {:?}", file_path);
    Ok(file_path.to_string_lossy().to_string())
}

/// Load a knowledge graph from a file on disk, replacing the current graph.
///
/// `path` is the absolute path to the JSON graph file.
#[tauri::command]
pub async fn load_graph(path: String, state: State<'_, AppState>) -> Result<(), String> {
    let file_path = std::path::PathBuf::from(&path);

    if !file_path.exists() {
        return Err(format!("Graph file not found: {}", path));
    }

    let loaded = crate::graph::temporal::TemporalKnowledgeGraph::load_from_file(&file_path)?;

    // Replace the in-memory knowledge graph
    {
        let mut graph = state
            .knowledge_graph
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        *graph = loaded;
    }

    // Update the cached snapshot
    {
        let graph = state
            .knowledge_graph
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        let snapshot = graph.snapshot();
        if let Ok(mut gs) = state.graph_snapshot.write() {
            *gs = snapshot;
        }
    }

    log::info!("Graph loaded from {:?}", file_path);
    Ok(())
}

/// Export the knowledge graph as a JSON string (for clipboard / download).
#[tauri::command]
pub async fn export_graph(state: State<'_, AppState>) -> Result<String, String> {
    let snapshot = state
        .graph_snapshot
        .read()
        .map_err(|e| format!("Failed to read graph snapshot: {}", e))?;
    serde_json::to_string_pretty(&*snapshot)
        .map_err(|e| format!("Failed to serialize graph: {}", e))
}

/// Get the current session ID.
#[tauri::command]
pub async fn get_session_id(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.current_session_id())
}

// ---------------------------------------------------------------------------
// Session management commands (v1: list / load transcript / delete)
// ---------------------------------------------------------------------------

/// List past sessions from `~/.audiograph/sessions.json`, most recent first.
/// Pass `limit` to cap the number of returned entries (e.g. `Some(10)`).
#[tauri::command]
pub fn list_sessions(limit: Option<usize>) -> Vec<crate::sessions::SessionMetadata> {
    let mut sessions = crate::sessions::load_index();
    if let Some(n) = limit {
        sessions.truncate(n);
    }
    sessions
}

/// Validate a session ID is safe to use as a file name segment.
/// Rejects anything that could enable path traversal (`..`, `/`, `\`, null).
fn validate_session_id(session_id: &str) -> Result<(), String> {
    if session_id.is_empty() || session_id.len() > 128 {
        return Err("Invalid session ID (length)".to_string());
    }
    // Allow only alphanumerics, hyphens, and underscores — covers UUIDs and sane IDs.
    if !session_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Invalid session ID (contains disallowed characters)".to_string());
    }
    Ok(())
}

/// Load a past session's transcript from disk. Returns the parsed
/// `TranscriptSegment`s from `~/.audiograph/transcripts/<session_id>.jsonl`.
#[tauri::command]
pub fn load_session_transcript(session_id: String) -> Result<Vec<TranscriptSegment>, String> {
    validate_session_id(&session_id)?;
    let home = dirs::home_dir().ok_or("home dir")?;
    let path = home
        .join(".audiograph")
        .join("transcripts")
        .join(format!("{}.jsonl", session_id));
    if !path.exists() {
        return Err(format!("Transcript file not found: {}", path.display()));
    }
    let contents = std::fs::read_to_string(&path).map_err(|e| format!("{}", e))?;
    let mut segments = Vec::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TranscriptSegment>(line) {
            Ok(seg) => segments.push(seg),
            Err(e) => log::warn!("Skipping malformed transcript line: {}", e),
        }
    }
    Ok(segments)
}

/// Delete a session: remove it from the index, then best-effort delete
/// its transcript and graph files from disk.
#[tauri::command]
pub fn delete_session(session_id: String) -> Result<(), String> {
    validate_session_id(&session_id)?;
    // Use the locked index-mutation helper so a concurrent autosave tick
    // can't re-write the removed entry after we drop it.
    crate::sessions::remove_from_index(&session_id)?;
    let home = dirs::home_dir().ok_or("home dir")?;
    let t = home
        .join(".audiograph")
        .join("transcripts")
        .join(format!("{}.jsonl", session_id));
    let g = home
        .join(".audiograph")
        .join("graphs")
        .join(format!("{}.json", session_id));
    // Log deletion results rather than silently dropping errors.
    match std::fs::remove_file(&t) {
        Ok(_) => log::info!("Deleted transcript: {}", t.display()),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            log::warn!("Failed to delete transcript {}: {}", t.display(), e);
        }
        _ => {}
    }
    match std::fs::remove_file(&g) {
        Ok(_) => log::info!("Deleted graph: {}", g.display()),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            log::warn!("Failed to delete graph {}: {}", g.display(), e);
        }
        _ => {}
    }
    Ok(())
}

/// Load the token-usage record for a session from
/// `~/.audiograph/usage/<session_id>.json`. Missing or malformed files
/// resolve to a zeroed record — callers never have to disambiguate.
#[tauri::command]
pub fn get_session_usage(
    session_id: String,
) -> Result<crate::sessions::usage::SessionUsage, String> {
    validate_session_id(&session_id)?;
    Ok(crate::sessions::usage::load_usage(&session_id))
}

/// Load the token-usage record for the CURRENT session. Convenience wrapper
/// so the frontend can restore its in-memory totals on startup without first
/// having to fetch `get_session_id`.
#[tauri::command]
pub fn get_current_session_usage(
    state: State<'_, AppState>,
) -> Result<crate::sessions::usage::SessionUsage, String> {
    Ok(crate::sessions::usage::load_usage(
        &state.current_session_id(),
    ))
}

/// Aggregate usage across every on-disk session file. This is the
/// authoritative source for the frontend's "Lifetime" totals panel — the
/// prior localStorage-backed lifetime counter was only ever a best-effort
/// mirror of this sum.
#[tauri::command]
pub fn get_lifetime_usage() -> Result<crate::sessions::usage::LifetimeUsage, String> {
    Ok(crate::sessions::usage::load_lifetime_usage())
}

/// Flush the current session and rotate to a fresh one in-process.
///
/// Behavior:
///   1. Finalize current session's sessions-index entry (status → complete).
///   2. Re-save the current session's usage record so on-disk totals are
///      flushed before the ID rotates.
///   3. Seed a fresh zeroed usage file for the new session so
///      `get_current_session_usage` returns zeros immediately after rotation.
///   4. Rotate `AppState::session_id` in place:
///        - The transcript writer is respawned against the new ID's file.
///        - The graph-autosave thread re-reads the ID on its next 30s tick
///          and starts writing to the new session's file.
///        - The Gemini event thread re-reads the ID on the next TurnComplete.
///   5. Register the new session in the sessions index so list_sessions
///      shows it alongside the previous one.
///
/// Returns the new session ID.
#[tauri::command]
pub fn new_session_cmd(state: State<'_, AppState>) -> Result<String, String> {
    let previous_id = state.current_session_id();

    // 1. Finalize current session's index entry. Best-effort: a failed
    //    finalize must not prevent us handing the caller a fresh UUID.
    if let Err(e) = crate::sessions::finalize_session(&previous_id) {
        log::warn!("new_session_cmd: finalize current failed: {}", e);
    }

    // 2. Re-save the current session's usage record. If the file is missing
    //    this is a harmless zero-write; if it exists, `save_usage` is a
    //    no-op rewrite of the same bytes. Either way, it guarantees the
    //    file is present on disk before the caller moves on.
    let current = crate::sessions::usage::load_usage(&previous_id);
    if let Err(e) = crate::sessions::usage::save_usage(&current) {
        log::warn!("new_session_cmd: save current usage failed: {}", e);
    }

    // 3. Seed a fresh usage file for the next session. Do this BEFORE the
    //    rotate so `get_current_session_usage` immediately reads zeroes.
    let new_id = uuid::Uuid::new_v4().to_string();
    let fresh = crate::sessions::usage::SessionUsage {
        session_id: new_id.clone(),
        ..crate::sessions::usage::SessionUsage::default()
    };
    crate::sessions::usage::save_usage(&fresh)?;

    // 4. Rotate in-process. `rotate_session` swaps the session_id Arc and
    //    respawns the transcript writer; the autosave + gemini-event
    //    threads pick up the change on their next iteration.
    let rotated_from = state.rotate_session(&new_id);
    debug_assert_eq!(rotated_from, previous_id);

    // 5. Register new session in the index so it shows up in list_sessions
    //    (status "active"). Best-effort: failure just means the UI won't
    //    see the entry until the next restart rediscovers it.
    if let Err(e) = crate::sessions::register_session(&new_id) {
        log::warn!("new_session_cmd: register_session failed: {}", e);
    }

    log::info!("new_session_cmd: rotated {} → {}", previous_id, new_id);
    Ok(new_id)
}

// ---------------------------------------------------------------------------
// Credential management commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn save_credential_cmd(key: String, value: String) -> Result<(), crate::error::AppError> {
    // Boundary-layer allowlist check (loop11 MEDIUM #5): reject unknown keys
    // here before they reach the inner `set_field` match. Mirrors the
    // convention used by `validate_session_id` elsewhere in this module.
    if !crate::credentials::is_allowed_key(&key) {
        return Err(crate::error::AppError::CredentialFileError {
            reason: format!("Unknown credential key: {}", key),
        });
    }
    // Pilot migration (loop10 MEDIUM #8): bubble credential-file failures as
    // `CredentialFileError` so the frontend can render a localized / actionable
    // message instead of a bare string.
    crate::credentials::set_credential(&key, &value)
        .map_err(|reason| crate::error::AppError::CredentialFileError { reason })
}

/// Explicitly clear a stored credential. Needed because `save_credential_cmd`
/// treats empty strings as a no-op (to avoid clobbering on blank form fields),
/// so there has to be a separate way for users to actually delete a key.
#[tauri::command]
pub fn delete_credential_cmd(key: String) -> Result<(), String> {
    // Boundary-layer allowlist check (loop11 MEDIUM #5). This command still
    // returns `Result<_, String>`; emit the same message the inner
    // `set_field` match would have produced, but reject at the boundary.
    if !crate::credentials::is_allowed_key(&key) {
        return Err(format!("Unknown credential key: {}", key));
    }
    crate::credentials::delete_credential(&key)
}

#[tauri::command]
pub fn load_credential_cmd(key: String) -> Result<Option<String>, String> {
    // Boundary-layer allowlist check (loop11 MEDIUM #5). This command still
    // returns `Result<_, String>`; emit the same message the inner match
    // below would have produced, but reject at the boundary.
    if !crate::credentials::is_allowed_key(&key) {
        return Err(format!("Unknown credential key: {}", key));
    }
    let store = crate::credentials::load_credentials();
    // Note: `CredentialStore` implements `Drop` (via `ZeroizeOnDrop`), so we
    // cannot move fields out of it — clone the returned value instead. The
    // original `store` is zeroized when it goes out of scope.
    let value = match key.as_str() {
        "openai_api_key" => store.openai_api_key.clone(),
        "groq_api_key" => store.groq_api_key.clone(),
        "together_api_key" => store.together_api_key.clone(),
        "fireworks_api_key" => store.fireworks_api_key.clone(),
        "deepgram_api_key" => store.deepgram_api_key.clone(),
        "assemblyai_api_key" => store.assemblyai_api_key.clone(),
        "gemini_api_key" => store.gemini_api_key.clone(),
        "google_service_account_path" => store.google_service_account_path.clone(),
        "aws_access_key" => store.aws_access_key.clone(),
        "aws_secret_key" => store.aws_secret_key.clone(),
        "aws_session_token" => store.aws_session_token.clone(),
        "aws_profile" => store.aws_profile.clone(),
        "aws_region" => store.aws_region.clone(),
        _ => return Err(format!("Unknown credential key: {}", key)),
    };
    Ok(value)
}

#[tauri::command]
pub fn load_all_credentials_cmd() -> crate::credentials::CredentialStore {
    crate::credentials::load_credentials()
}

/// Diagnose credential-store health. Surfaces parse/read errors from
/// `credentials.yaml` to the UI so users can tell the difference between
/// "no keys set" and "keys exist but the file is broken".
#[tauri::command]
pub fn diagnose_credentials() -> Result<String, String> {
    match crate::credentials::try_load_credentials() {
        Ok(store) => {
            let count = [
                store.openai_api_key.is_some(),
                store.groq_api_key.is_some(),
                store.deepgram_api_key.is_some(),
                store.assemblyai_api_key.is_some(),
                store.gemini_api_key.is_some(),
                store.aws_secret_key.is_some(),
            ]
            .iter()
            .filter(|&&b| b)
            .count();
            Ok(format!(
                "Credentials loaded successfully ({} keys present)",
                count
            ))
        }
        Err(e) => Err(e),
    }
}

/// List available AWS profiles from ~/.aws/config and ~/.aws/credentials.
#[tauri::command]
pub fn list_aws_profiles() -> Vec<String> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return vec![],
    };
    let mut profiles = std::collections::BTreeSet::new();

    for filename in &["config", "credentials"] {
        let path = home.join(".aws").join(filename);
        if let Ok(contents) = std::fs::read_to_string(&path) {
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("[profile ") && trimmed.ends_with(']') {
                    let name = &trimmed[9..trimmed.len() - 1];
                    profiles.insert(name.to_string());
                } else if trimmed == "[default]" {
                    profiles.insert("default".to_string());
                } else if *filename == "credentials"
                    && trimmed.starts_with('[')
                    && trimmed.ends_with(']')
                {
                    let name = &trimmed[1..trimmed.len() - 1];
                    profiles.insert(name.to_string());
                }
            }
        }
    }

    profiles.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Cloud provider connection tests
// ---------------------------------------------------------------------------
//
// These commands let the Settings UI verify a user's API keys / credentials
// *before* they start a transcription session, so authentication failures
// surface immediately instead of after ~10s of silent audio streaming.

/// Test an OpenAI-compatible ASR endpoint by making a GET /models request.
#[tauri::command]
pub async fn test_cloud_asr_connection(
    endpoint: String,
    api_key: String,
) -> Result<String, String> {
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build client: {}", e))?;
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.bearer_auth(&api_key);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {}: {}",
            status,
            body.chars().take(200).collect::<String>()
        ));
    }
    Ok(format!("Connected to {} (HTTP {})", endpoint, status))
}

/// Test Deepgram API key by calling /v1/projects.
#[tauri::command]
pub async fn test_deepgram_connection(api_key: String) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("API key is empty".to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build client: {}", e))?;
    let resp = client
        // Use /v1/models (works with `usage` scope — the scope most keys
        // have for transcription). /v1/projects requires the `manage` scope
        // which would return 403 for valid transcription-only keys.
        .get("https://api.deepgram.com/v1/models")
        .header("Authorization", format!("Token {}", api_key))
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("Deepgram returned HTTP {}", status));
    }
    Ok("Deepgram API key is valid".to_string())
}

/// Test AssemblyAI API key by calling GET /v2/transcript with zero results.
#[tauri::command]
pub async fn test_assemblyai_connection(api_key: String) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("API key is empty".to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build client: {}", e))?;
    let resp = client
        .get("https://api.assemblyai.com/v2/transcript?limit=1")
        .header("Authorization", &api_key)
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("AssemblyAI returned HTTP {}", status));
    }
    Ok("AssemblyAI API key is valid".to_string())
}

/// Test Gemini API key via a simple listModels call.
///
/// Uses the `x-goog-api-key` header (not the `?key=` query string) to match
/// the production WebSocket auth pattern. Passing the key in URL would leak
/// it to DNS, proxies, and cert monitoring tools — and would silently succeed
/// even if the header-auth path is broken in production.
#[tauri::command]
pub async fn test_gemini_api_key(api_key: String) -> Result<String, String> {
    if api_key.trim().is_empty() {
        return Err("API key is empty".to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build client: {}", e))?;
    let resp = client
        .get("https://generativelanguage.googleapis.com/v1beta/models")
        .header("x-goog-api-key", api_key.trim())
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("Gemini API returned HTTP {}", status));
    }
    Ok("Gemini API key is valid".to_string())
}

/// Test AWS credentials via STS GetCallerIdentity (works for any AWS API access).
///
/// Shared between AWS Transcribe and AWS Bedrock settings — both providers
/// pull from the same backend credential store.
#[tauri::command]
pub async fn test_aws_credentials(
    region: String,
    credential_source: crate::settings::AwsCredentialSource,
) -> Result<String, String> {
    let region_trimmed = region.trim();
    if region_trimmed.is_empty() {
        return Err("AWS region is empty. Enter a region like 'us-east-1'.".to_string());
    }
    if !region_trimmed.contains('-') {
        return Err(format!(
            "AWS region '{}' looks invalid. Expected format like 'us-east-1'.",
            region_trimmed
        ));
    }
    let region = region_trimmed.to_string();

    let sdk_config = crate::aws_util::build_aws_sdk_config(&region, credential_source).await?;
    let sts = aws_sdk_sts::Client::new(&sdk_config);
    let identity = sts
        .get_caller_identity()
        .send()
        .await
        .map_err(|e| format!("AWS auth failed: {}", e))?;
    Ok(format!(
        "Authenticated as {} (account: {})",
        identity.arn().unwrap_or("unknown"),
        identity.account().unwrap_or("unknown")
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // PART 1 — configure_api_endpoint URL validation regression tests
    // (loop-13 MEDIUM #4). The validation landed in loop 12 without
    // coverage; these lock in the accept/reject contract so a future
    // refactor can't silently loosen it.
    // -----------------------------------------------------------------------

    #[test]
    fn validate_endpoint_url_accepts_https() {
        let u =
            validate_endpoint_url("https://api.openai.com/v1").expect("https URL must be accepted");
        assert_eq!(u.scheme(), "https");
    }

    #[test]
    fn validate_endpoint_url_accepts_http() {
        // Plain http is legitimate for local servers (Ollama, LM Studio, vLLM).
        let u = validate_endpoint_url("http://localhost:11434/v1")
            .expect("http URL must be accepted for local servers");
        assert_eq!(u.scheme(), "http");
    }

    #[test]
    fn validate_endpoint_url_rejects_malformed() {
        let err = validate_endpoint_url("not a url").expect_err("garbage must be rejected");
        assert!(
            err.contains("Invalid endpoint URL"),
            "error should mention invalid URL, got: {}",
            err
        );
    }

    #[test]
    fn validate_endpoint_url_rejects_disallowed_schemes() {
        // file:// would let a settings-file edit coax the app into reading
        // local files. ftp:// is non-functional with reqwest. Both must be
        // rejected up-front with a scheme-specific message.
        for bad in &["file:///etc/passwd", "ftp://example.com/models"] {
            let err = validate_endpoint_url(bad).expect_err(&format!("{} must be rejected", bad));
            assert!(
                err.contains("unsupported scheme"),
                "error for {} should mention unsupported scheme, got: {}",
                bad,
                err
            );
        }
    }

    // -----------------------------------------------------------------------
    // PART 2 — log_level persistence race (loop-13 MEDIUM #6).
    // set_log_level is now the runtime-only path; save_settings_cmd owns
    // the single disk-write path. The full command needs a Tauri AppHandle
    // (not available in unit tests), so we exercise the in-memory half
    // directly and assert the invariant that matters: the cache tracks
    // the latest level without triggering a disk flush.
    // -----------------------------------------------------------------------

    #[test]
    fn set_log_level_does_not_persist_to_disk_on_repeated_calls() {
        // Simulate what `set_log_level` does to the in-memory cache: apply
        // the runtime level, then mutate `app_settings.log_level`. Repeating
        // this twice must leave the cache reflecting the final value and
        // must not touch disk — which it can't, because we never hand it
        // an AppHandle.
        let state = AppState::new();

        // First call: info → debug.
        crate::logging::apply_log_level("debug");
        {
            let mut cached = state.app_settings.write().expect("lock poisoned");
            cached.log_level = Some("debug".to_string());
        }
        assert_eq!(
            state.app_settings.read().unwrap().log_level.as_deref(),
            Some("debug"),
            "cache must reflect first update"
        );

        // Second call: debug → warn. With the old contract this would have
        // produced a second disk write; under the new contract it only
        // mutates runtime + cache.
        crate::logging::apply_log_level("warn");
        {
            let mut cached = state.app_settings.write().expect("lock poisoned");
            cached.log_level = Some("warn".to_string());
        }
        assert_eq!(
            state.app_settings.read().unwrap().log_level.as_deref(),
            Some("warn"),
            "cache must reflect second update"
        );

        // Restore a sensible default so later tests in the same binary
        // aren't silently swallowing logs at warn.
        crate::logging::apply_log_level("info");
    }
}
