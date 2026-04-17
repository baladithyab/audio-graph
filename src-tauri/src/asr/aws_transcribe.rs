//! AWS Transcribe Streaming ASR integration.
//!
//! Uses the aws-sdk-transcribestreaming crate to stream audio to AWS
//! and receive real-time transcription results with optional speaker diarization.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_transcribestreaming as transcribe;
use aws_sdk_transcribestreaming::primitives::Blob;
use aws_sdk_transcribestreaming::types::{AudioEvent, AudioStream, MediaEncoding};
use crossbeam_channel::Receiver;
use uuid::Uuid;

use crate::audio::pipeline::ProcessedAudioChunk;
use crate::settings::AwsCredentialSource;
use crate::state::TranscriptSegment;

pub struct AwsTranscribeConfig {
    pub region: String,
    pub language_code: String,
    pub credential_source: AwsCredentialSource,
    pub enable_diarization: bool,
}

fn f32_to_pcm_bytes(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let i16_val = (clamped * 32767.0) as i16;
        bytes.extend_from_slice(&i16_val.to_le_bytes());
    }
    bytes
}

async fn build_aws_config(
    config: &AwsTranscribeConfig,
) -> Result<aws_config::SdkConfig, String> {
    let region = aws_config::Region::new(config.region.clone());
    match &config.credential_source {
        AwsCredentialSource::DefaultChain => Ok(aws_config::defaults(BehaviorVersion::latest())
            .region(region)
            .load()
            .await),
        AwsCredentialSource::Profile { name } => {
            Ok(aws_config::defaults(BehaviorVersion::latest())
                .profile_name(name)
                .region(region)
                .load()
                .await)
        }
        AwsCredentialSource::AccessKeys { access_key } => {
            let cred_store = crate::credentials::load_credentials();
            // `CredentialStore` implements `Drop` (via `ZeroizeOnDrop`), so
            // fields must be cloned rather than moved out.
            let secret_key = cred_store
                .aws_secret_key
                .clone()
                .ok_or("AWS secret key not found in credentials store")?;
            let session_token = cred_store.aws_session_token.clone();
            let creds = Credentials::new(
                access_key,
                &secret_key,
                session_token,
                None,
                "audio-graph",
            );
            Ok(aws_config::defaults(BehaviorVersion::latest())
                .credentials_provider(creds)
                .region(region)
                .load()
                .await)
        }
    }
}

/// Run an AWS Transcribe streaming session. Blocking — meant for a dedicated thread.
///
/// Reads ProcessedAudioChunks from the receiver, streams them to AWS Transcribe,
/// and returns TranscriptSegments via the provided callback.
pub fn run_aws_transcribe_session(
    audio_rx: Receiver<ProcessedAudioChunk>,
    is_transcribing: Arc<AtomicBool>,
    config: AwsTranscribeConfig,
    on_transcript: impl FnMut(TranscriptSegment) + Send + 'static,
) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to create tokio runtime: {}", e))?;

    rt.block_on(async {
        run_streaming_session(audio_rx, is_transcribing, config, on_transcript).await
    })
}

async fn run_streaming_session(
    audio_rx: Receiver<ProcessedAudioChunk>,
    is_transcribing: Arc<AtomicBool>,
    config: AwsTranscribeConfig,
    mut on_transcript: impl FnMut(TranscriptSegment) + Send + 'static,
) -> Result<(), String> {
    let sdk_config = build_aws_config(&config).await?;
    let client = transcribe::Client::new(&sdk_config);

    let (audio_tx, audio_stream_rx) =
        tokio::sync::mpsc::channel::<Result<AudioStream, transcribe::types::error::AudioStreamError>>(16);

    let audio_stream: aws_smithy_http::event_stream::EventStreamSender<
        AudioStream,
        transcribe::types::error::AudioStreamError,
    > = aws_smithy_http::event_stream::EventStreamSender::from(
        tokio_stream::wrappers::ReceiverStream::new(audio_stream_rx),
    );

    let language_code = config
        .language_code
        .parse::<transcribe::types::LanguageCode>()
        .unwrap_or(transcribe::types::LanguageCode::EnUs);

    let mut builder = client
        .start_stream_transcription()
        .language_code(language_code)
        .media_sample_rate_hertz(16000)
        .media_encoding(MediaEncoding::Pcm)
        .audio_stream(audio_stream);

    if config.enable_diarization {
        builder = builder.show_speaker_label(true);
    }

    let mut output = builder
        .send()
        .await
        .map_err(|e| format!("Failed to start AWS Transcribe stream: {}", e))?;

    log::info!("AWS Transcribe: streaming session started");

    let is_transcribing_sender = is_transcribing.clone();
    tokio::spawn(async move {
        loop {
            if !is_transcribing_sender.load(Ordering::Relaxed) {
                break;
            }

            match audio_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(chunk) => {
                    let pcm_bytes = f32_to_pcm_bytes(&chunk.data);
                    let audio_event = AudioEvent::builder()
                        .audio_chunk(Blob::new(pcm_bytes))
                        .build();
                    if audio_tx
                        .send(Ok(AudioStream::AudioEvent(audio_event)))
                        .await
                        .is_err()
                    {
                        log::info!("AWS Transcribe: audio channel closed");
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    log::info!("AWS Transcribe: audio source disconnected");
                    break;
                }
            }
        }
        drop(audio_tx);
    });

    while let Some(event) = output
        .transcript_result_stream
        .recv()
        .await
        .map_err(|e| format!("AWS Transcribe stream error: {}", e))?
    {
        if !is_transcribing.load(Ordering::Relaxed) {
            break;
        }

        if let transcribe::types::TranscriptResultStream::TranscriptEvent(ev) = event {
            if let Some(transcript) = ev.transcript {
                for result in transcript.results.unwrap_or_default() {
                    if result.is_partial() {
                        continue;
                    }

                    let result_start = result.start_time();
                    let result_end = result.end_time();

                    for alt in result.alternatives.unwrap_or_default() {
                        let text = alt.transcript.unwrap_or_default();
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            continue;
                        }

                        let speaker_label = alt
                            .items
                            .as_ref()
                            .and_then(|items| {
                                items
                                    .iter()
                                    .find_map(|item| item.speaker().map(|s| s.to_string()))
                            });

                        let confidence = alt
                            .items
                            .as_ref()
                            .and_then(|items| {
                                let confs: Vec<f32> = items
                                    .iter()
                                    .filter_map(|i| i.confidence().map(|c| c as f32))
                                    .collect();
                                if confs.is_empty() {
                                    None
                                } else {
                                    Some(confs.iter().sum::<f32>() / confs.len() as f32)
                                }
                            })
                            .unwrap_or(0.9);

                        let segment = TranscriptSegment {
                            id: Uuid::new_v4().to_string(),
                            source_id: String::new(),
                            speaker_id: speaker_label.clone(),
                            speaker_label,
                            text,
                            start_time: result_start,
                            end_time: result_end,
                            confidence,
                        };

                        on_transcript(segment);
                    }
                }
            }
        }
    }

    log::info!("AWS Transcribe: streaming session ended");
    Ok(())
}
