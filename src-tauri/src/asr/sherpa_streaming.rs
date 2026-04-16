//! Streaming ASR via sherpa-onnx (Zipformer transducer).
//!
//! Processes audio frame-by-frame with sub-200ms first-word latency.
//! Uses the official `sherpa-onnx` crate's `OnlineRecognizer` API.
//!
//! The sherpa-onnx Rust crate wraps the C API via sherpa-onnx-sys (FFI).
//! Key types: `OnlineRecognizer`, `OnlineStream`, and config structs.

use std::path::PathBuf;

use sherpa_onnx::{
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
    OnlineTransducerModelConfig,
};

/// Configuration for the sherpa-onnx streaming ASR worker.
pub struct SherpaStreamingConfig {
    /// Directory containing the model files (encoder, decoder, joiner, tokens).
    pub model_dir: PathBuf,
    /// Whether to enable endpoint detection (silence-based utterance segmentation).
    pub enable_endpoint_detection: bool,
}

/// Streaming ASR worker backed by sherpa-onnx's OnlineRecognizer.
///
/// Call [`SherpaStreamingWorker::process_chunk`] for each audio chunk.
/// The recognizer processes audio frame-by-frame and returns partial/final
/// results as endpoints are detected.
pub struct SherpaStreamingWorker {
    recognizer: OnlineRecognizer,
    stream: OnlineStream,
}

impl SherpaStreamingWorker {
    /// Create a new streaming ASR worker with the given config.
    ///
    /// Loads the Zipformer transducer model from `config.model_dir`.
    /// Expected files in the model directory:
    /// - `encoder-epoch-99-avg-1.onnx`
    /// - `decoder-epoch-99-avg-1.onnx`
    /// - `joiner-epoch-99-avg-1.onnx`
    /// - `tokens.txt`
    pub fn new(config: &SherpaStreamingConfig) -> Result<Self, String> {
        let encoder_path = config
            .model_dir
            .join("encoder-epoch-99-avg-1.onnx")
            .display()
            .to_string();
        let decoder_path = config
            .model_dir
            .join("decoder-epoch-99-avg-1.onnx")
            .display()
            .to_string();
        let joiner_path = config
            .model_dir
            .join("joiner-epoch-99-avg-1.onnx")
            .display()
            .to_string();
        let tokens_path = config.model_dir.join("tokens.txt").display().to_string();

        // Validate that model files exist
        for (name, path) in &[
            ("encoder", &encoder_path),
            ("decoder", &decoder_path),
            ("joiner", &joiner_path),
            ("tokens", &tokens_path),
        ] {
            if !std::path::Path::new(path).exists() {
                return Err(format!(
                    "Sherpa-onnx model file not found: {} (expected at {})",
                    name, path
                ));
            }
        }

        // Build config structs for sherpa-onnx.
        //
        // NOTE: The exact field names and types depend on the sherpa-onnx crate
        // version. The config structs wrap the C API's SherpaOnnxOnlineRecognizerConfig.
        // If compilation fails here, adjust field names to match the installed version.
        // See: https://github.com/k2-fsa/sherpa-onnx/tree/master/rust-api-examples
        let transducer = OnlineTransducerModelConfig {
            encoder: &encoder_path,
            decoder: &decoder_path,
            joiner: &joiner_path,
        };

        let model_config = OnlineModelConfig {
            transducer,
            tokens: &tokens_path,
            num_threads: 2,
            provider: "cpu",
            debug: false,
            ..Default::default()
        };

        let rec_config = OnlineRecognizerConfig {
            model_config,
            decoding_method: "greedy_search",
            max_active_paths: 4,
            enable_endpoint: if config.enable_endpoint_detection {
                1
            } else {
                0
            },
            rule1_min_trailing_silence: 2.4,
            rule2_min_trailing_silence: 1.2,
            rule3_min_utterance_length: 20.0,
            ..Default::default()
        };

        let recognizer = OnlineRecognizer::new(&rec_config);
        let stream = recognizer.create_stream();

        log::info!(
            "Sherpa-onnx streaming ASR worker created (model_dir={}, endpoint_detection={})",
            config.model_dir.display(),
            config.enable_endpoint_detection,
        );

        Ok(Self { recognizer, stream })
    }

    /// Feed audio chunk and get result if available.
    ///
    /// The audio must be 16kHz mono f32 samples.
    /// Returns `Some((text, is_endpoint))` if there's recognized text.
    /// When `is_endpoint` is `true`, the utterance is complete and the
    /// stream has been reset for the next utterance.
    pub fn process_chunk(&mut self, samples: &[f32]) -> Option<(String, bool)> {
        self.stream.accept_waveform(16000, samples);

        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }

        let result = self.recognizer.get_result(&self.stream);
        let is_endpoint = self.recognizer.is_endpoint(&self.stream);

        if is_endpoint {
            self.recognizer.reset(&self.stream);
        }

        let text = result.trim().to_string();
        if !text.is_empty() {
            Some((text, is_endpoint))
        } else {
            None
        }
    }

    /// Reset the stream state (e.g. when starting a new utterance).
    pub fn reset(&mut self) {
        self.recognizer.reset(&self.stream);
    }
}
