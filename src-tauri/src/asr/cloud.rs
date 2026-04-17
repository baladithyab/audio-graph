use uuid::Uuid;

use crate::state::TranscriptSegment;

use super::SpeechSegment;

/// Cloud ASR provider configuration.
#[derive(Debug, Clone)]
pub struct CloudAsrConfig {
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    pub language: String,
}

/// Result from a cloud ASR transcription call.
#[derive(Debug, serde::Deserialize)]
struct WhisperResponse {
    text: String,
    #[serde(default)]
    segments: Option<Vec<WhisperSegment>>,
}

#[derive(Debug, serde::Deserialize)]
struct WhisperSegment {
    #[serde(default)]
    start: f64,
    #[serde(default)]
    end: f64,
    text: String,
    #[serde(default)]
    no_speech_prob: Option<f64>,
}

/// Encode 16kHz mono f32 audio samples into a WAV byte buffer (PCM s16le).
fn encode_wav(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let num_samples = samples.len();
    let bytes_per_sample: u16 = 2;
    let data_size = (num_samples * bytes_per_sample as usize) as u32;
    let file_size = 36 + data_size;

    let mut buf = Vec::with_capacity(44 + data_size as usize);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * channels as u32 * bytes_per_sample as u32;
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    let block_align = channels * bytes_per_sample;
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes()); // bits per sample

    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let i16_val = (clamped * 32767.0) as i16;
        buf.extend_from_slice(&i16_val.to_le_bytes());
    }

    buf
}

/// Transcribe a speech segment using an OpenAI-compatible STT API.
///
/// Works with: OpenAI Whisper API, Groq, Together AI, Deepgram (REST),
/// and any provider that implements the `/v1/audio/transcriptions` endpoint.
///
/// NOTE: This call blocks the calling thread for the full round-trip to the
/// API (typically 0.5–5s depending on provider and audio length). Callers
/// that dispatch segments at real-time rates should budget for this latency
/// (the upstream `AccumulatedSegment` channel capacity must absorb the
/// in-flight segment plus any queued segments produced while the HTTP call
/// is in flight).
pub fn transcribe_segment(
    config: &CloudAsrConfig,
    segment: &SpeechSegment,
) -> Result<Vec<TranscriptSegment>, String> {
    let call_start = std::time::Instant::now();
    let audio_secs = segment.audio.len() as f64 / 16_000.0;
    log::info!(
        "Cloud ASR: starting transcription request (audio={:.2}s, model={})",
        audio_secs,
        config.model
    );

    let wav_bytes = encode_wav(&segment.audio, 16000, 1);

    let url = format!(
        "{}/audio/transcriptions",
        config.endpoint.trim_end_matches('/')
    );

    let part = reqwest::blocking::multipart::Part::bytes(wav_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| format!("Failed to create multipart part: {}", e))?;

    let form = reqwest::blocking::multipart::Form::new()
        .part("file", part)
        .text("model", config.model.clone())
        .text("response_format", "verbose_json")
        .text("language", config.language.clone());

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let mut request = client.post(&url).multipart(form);
    if !config.api_key.is_empty() {
        request = request.bearer_auth(&config.api_key);
    }

    let response = request
        .send()
        .map_err(|e| format!("Cloud ASR request failed: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .unwrap_or_else(|_| "unable to read response body".to_string());
        return Err(format!("Cloud ASR API error ({}): {}", status, body));
    }

    let body = response
        .text()
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    let whisper_resp: WhisperResponse =
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse response: {}", e))?;

    let elapsed_ms = call_start.elapsed().as_millis();
    let rtf = call_start.elapsed().as_secs_f64() / audio_secs.max(0.001);
    if elapsed_ms > 2_000 {
        log::warn!(
            "Cloud ASR: slow API response — elapsed={}ms, audio={:.2}s, RTF={:.2}x (API slower than real-time, segments may be dropped)",
            elapsed_ms,
            audio_secs,
            rtf
        );
    } else {
        log::info!(
            "Cloud ASR: transcription complete — elapsed={}ms, audio={:.2}s, RTF={:.2}x",
            elapsed_ms,
            audio_secs,
            rtf
        );
    }

    let segment_start_secs = segment.start_time.as_secs_f64();

    if let Some(segments) = whisper_resp.segments {
        let transcripts: Vec<TranscriptSegment> = segments
            .into_iter()
            .filter(|s| !s.text.trim().is_empty())
            .map(|s| {
                let confidence = s
                    .no_speech_prob
                    .map(|p| (1.0 - p) as f32)
                    .unwrap_or(0.9);
                TranscriptSegment {
                    id: Uuid::new_v4().to_string(),
                    source_id: segment.source_id.clone(),
                    speaker_id: None,
                    speaker_label: None,
                    text: s.text.trim().to_string(),
                    start_time: segment_start_secs + s.start,
                    end_time: segment_start_secs + s.end,
                    confidence,
                }
            })
            .collect();
        Ok(transcripts)
    } else {
        let text = whisper_resp.text.trim().to_string();
        if text.is_empty() {
            return Ok(vec![]);
        }
        Ok(vec![TranscriptSegment {
            id: Uuid::new_v4().to_string(),
            source_id: segment.source_id.clone(),
            speaker_id: None,
            speaker_label: None,
            text,
            start_time: segment_start_secs,
            end_time: segment.end_time.as_secs_f64(),
            confidence: 0.9,
        }])
    }
}
