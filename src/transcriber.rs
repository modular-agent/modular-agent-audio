use modular_agent_core::{Error, Result};

/// Speech-to-text backend. Created and used on the inference thread.
pub(crate) trait Transcriber {
    fn transcribe(&mut self, samples_16k: &[f32], language: &str) -> Result<String>;
}

#[cfg(feature = "transcribe")]
const ENGINE_WHISPER: &str = "whisper";
#[cfg(feature = "sherpa")]
const ENGINE_SHERPA: &str = "sherpa";

const AVAILABLE_ENGINES: &[&str] = &[
    #[cfg(feature = "transcribe")]
    ENGINE_WHISPER,
    #[cfg(feature = "sherpa")]
    ENGINE_SHERPA,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Engine {
    #[cfg(feature = "transcribe")]
    Whisper,
    #[cfg(feature = "sherpa")]
    Sherpa,
}

impl Engine {
    /// Empty selects Whisper when it is compiled in, otherwise sherpa-onnx.
    pub(crate) fn parse(name: &str) -> Result<Self> {
        match name {
            "" => Ok(Self::default_engine()),
            #[cfg(feature = "transcribe")]
            ENGINE_WHISPER => Ok(Self::Whisper),
            #[cfg(feature = "sherpa")]
            ENGINE_SHERPA => Ok(Self::Sherpa),
            other => Err(Error::InvalidConfig(format!(
                "Transcription engine '{}' is not available in this build. Available: {}",
                other,
                AVAILABLE_ENGINES.join(", ")
            ))),
        }
    }

    fn default_engine() -> Self {
        #[cfg(feature = "transcribe")]
        {
            Self::Whisper
        }
        #[cfg(not(feature = "transcribe"))]
        {
            Self::Sherpa
        }
    }
}

/// Everything the inference thread needs to build a `Transcriber`.
/// File existence is validated before this is constructed: sherpa-onnx
/// terminates the process on a missing model file instead of returning an error.
pub(crate) enum EngineSpec {
    #[cfg(feature = "transcribe")]
    Whisper { model_path: String },
    #[cfg(feature = "sherpa")]
    Sherpa {
        files: crate::engine_sherpa::SherpaModelFiles,
    },
}

impl EngineSpec {
    pub(crate) fn load(self, n_threads: i32) -> Result<Box<dyn Transcriber>> {
        match self {
            #[cfg(feature = "transcribe")]
            EngineSpec::Whisper { model_path } => Ok(Box::new(
                crate::engine_whisper::WhisperTranscriber::new(&model_path, n_threads)?,
            )),
            #[cfg(feature = "sherpa")]
            EngineSpec::Sherpa { files } => Ok(Box::new(
                crate::engine_sherpa::SherpaTranscriber::new(&files, n_threads)?,
            )),
        }
    }
}

/// Half the logical cores, clamped to 2..=8: leaves room for the audio path
/// and the rest of the app while still using multi-core CPUs for inference.
pub(crate) fn infer_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(2)
        .clamp(2, 8) as i32
}
