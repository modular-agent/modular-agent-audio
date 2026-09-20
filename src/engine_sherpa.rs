use std::path::{Path, PathBuf};

use modular_agent_core::{Error, Result};
use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
};

use crate::transcriber::Transcriber;

pub(crate) const SAMPLE_RATE: i32 = 16000;
pub(crate) const MODEL_DOWNLOAD_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-zipformer-ja-en-reazonspeech-2025-01-17.tar.bz2";

/// Resolved paths of a sherpa-onnx transducer model directory.
pub(crate) struct SherpaModelFiles {
    encoder: PathBuf,
    decoder: PathBuf,
    joiner: PathBuf,
    tokens: PathBuf,
}

impl SherpaModelFiles {
    /// Locates encoder/decoder/joiner (int8 preferred, then fp32, then fp16)
    /// and `tokens.txt` in `dir`, verifying each exists.
    pub(crate) fn locate(dir: &Path) -> Result<Self> {
        let entries = std::fs::read_dir(dir).map_err(|e| {
            Error::InvalidConfig(format!(
                "Cannot read sherpa_model_dir '{}': {}. Download from {}",
                dir.display(),
                e,
                MODEL_DOWNLOAD_URL
            ))
        })?;
        let names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();

        let pick = |part: &str| -> Result<PathBuf> {
            let rank = |name: &str| {
                if name.contains("int8") {
                    0
                } else if name.contains("fp16") {
                    2
                } else {
                    1
                }
            };
            names
                .iter()
                .filter(|n| n.starts_with(part) && n.ends_with(".onnx"))
                .min_by_key(|n| rank(n))
                .map(|n| dir.join(n))
                .ok_or_else(|| {
                    Error::InvalidConfig(format!(
                        "No {}*.onnx in sherpa_model_dir '{}'. Download from {}",
                        part,
                        dir.display(),
                        MODEL_DOWNLOAD_URL
                    ))
                })
        };

        let tokens = dir.join("tokens.txt");
        if !tokens.is_file() {
            return Err(Error::InvalidConfig(format!(
                "tokens.txt not found in sherpa_model_dir '{}'",
                dir.display()
            )));
        }
        Ok(Self {
            encoder: pick("encoder")?,
            decoder: pick("decoder")?,
            joiner: pick("joiner")?,
            tokens,
        })
    }
}

pub(crate) struct SherpaTranscriber {
    recognizer: OfflineRecognizer,
}

impl SherpaTranscriber {
    pub(crate) fn new(files: &SherpaModelFiles, n_threads: i32) -> Result<Self> {
        let path = |p: &Path| Some(p.to_string_lossy().into_owned());
        log::info!(
            "Loading sherpa-onnx transducer '{}' ({} threads)",
            files.encoder.display(),
            n_threads
        );
        let config = OfflineRecognizerConfig {
            model_config: OfflineModelConfig {
                transducer: OfflineTransducerModelConfig {
                    encoder: path(&files.encoder),
                    decoder: path(&files.decoder),
                    joiner: path(&files.joiner),
                },
                tokens: path(&files.tokens),
                num_threads: n_threads,
                provider: Some("cpu".into()),
                model_type: Some("transducer".into()),
                ..Default::default()
            },
            decoding_method: Some("greedy_search".into()),
            ..Default::default()
        };
        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            Error::IoError(format!(
                "Failed to create sherpa-onnx recognizer from '{}'",
                files.encoder.display()
            ))
        })?;
        Ok(Self { recognizer })
    }
}

impl Transcriber for SherpaTranscriber {
    /// `language` is ignored: the model decides.
    fn transcribe(&mut self, samples_16k: &[f32], _language: &str) -> Result<String> {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE, samples_16k);
        self.recognizer.decode(&stream);
        Ok(stream
            .get_result()
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default())
    }
}
