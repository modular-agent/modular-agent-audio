use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use modular_agent_core::{Error, Result};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::transcriber::Transcriber;

// WhisperContext cache (shared across instances, keyed by model path)
static WHISPER_CONTEXT_MAP: OnceLock<Mutex<BTreeMap<String, Arc<WhisperContext>>>> =
    OnceLock::new();

fn get_whisper_context_map() -> &'static Mutex<BTreeMap<String, Arc<WhisperContext>>> {
    WHISPER_CONTEXT_MAP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn get_or_load_whisper_context(model_path: &str) -> Result<Arc<WhisperContext>> {
    let mut map = get_whisper_context_map().lock().unwrap();
    if let Some(ctx) = map.get(model_path) {
        return Ok(ctx.clone());
    }
    let params = WhisperContextParameters::default();
    log::info!(
        "Loading Whisper model '{}' (GPU: {})",
        model_path,
        cfg!(feature = "_gpu")
    );
    let ctx = WhisperContext::new_with_params(model_path, params).map_err(|e| {
        Error::IoError(format!(
            "Failed to load Whisper model '{}': {}",
            model_path, e
        ))
    })?;
    let ctx = Arc::new(ctx);
    map.insert(model_path.to_string(), ctx.clone());
    Ok(ctx)
}

pub(crate) struct WhisperTranscriber {
    state: WhisperState,
    n_threads: i32,
}

impl WhisperTranscriber {
    pub(crate) fn new(model_path: &str, n_threads: i32) -> Result<Self> {
        let ctx = get_or_load_whisper_context(model_path)?;
        let state = ctx
            .create_state()
            .map_err(|e| Error::IoError(format!("Failed to create whisper state: {}", e)))?;
        Ok(Self { state, n_threads })
    }
}

impl Transcriber for WhisperTranscriber {
    fn transcribe(&mut self, samples_16k: &[f32], language: &str) -> Result<String> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(language));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_single_segment(true);
        params.set_no_context(true);
        params.set_n_threads(self.n_threads);

        self.state
            .full(params, samples_16k)
            .map_err(|e| Error::IoError(format!("Whisper inference failed: {}", e)))?;

        let n_segments = self.state.full_n_segments();
        let mut text = String::new();
        for i in 0..n_segments {
            if let Some(segment) = self.state.get_segment(i)
                && let Ok(s) = segment.to_str_lossy()
            {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    text.push_str(trimmed);
                }
            }
        }
        Ok(text)
    }
}
