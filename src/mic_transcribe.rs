use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use modular_agent_core::{
    Agent, AgentContext, AgentData, AgentError, AgentSpec, AgentStatus, AgentValue, AsAgent,
    ModularAgent, async_trait, modular_agent,
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::vad::EnergyVad;

const CATEGORY: &str = "Audio";
const WHISPER_SAMPLE_RATE: u32 = 16000;

const PORT_TEXT: &str = "text";
const PORT_STATUS: &str = "status";

const CONFIG_ENABLED: &str = "enabled";
const CONFIG_DEVICE: &str = "device";
const CONFIG_LANGUAGE: &str = "language";
const CONFIG_VAD_SENSITIVITY: &str = "vad_sensitivity";
const CONFIG_MIN_VOLUME: &str = "min_volume";
const CONFIG_MAX_SEGMENT_DURATION: &str = "max_segment_duration";
const CONFIG_MODEL_PATH: &str = "model_path";

enum Command {
    Pause,
    Resume,
    Shutdown,
}

// WhisperContext cache (shared across instances, keyed by model path)
static WHISPER_CONTEXT_MAP: OnceLock<Mutex<BTreeMap<String, Arc<WhisperContext>>>> =
    OnceLock::new();

fn get_whisper_context_map() -> &'static Mutex<BTreeMap<String, Arc<WhisperContext>>> {
    WHISPER_CONTEXT_MAP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn get_or_load_whisper_context(model_path: &str) -> Result<Arc<WhisperContext>, AgentError> {
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
        AgentError::IoError(format!(
            "Failed to load Whisper model '{}': {}",
            model_path, e
        ))
    })?;
    let ctx = Arc::new(ctx);
    map.insert(model_path.to_string(), ctx.clone());
    Ok(ctx)
}

fn get_model_path(ma: &ModularAgent) -> Result<String, AgentError> {
    ma.get_global_configs(MicTranscribeAgent::DEF_NAME)
        .and_then(|cfg| cfg.get_string(CONFIG_MODEL_PATH).ok())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            AgentError::InvalidConfig(
                "Whisper model path not set. Download from https://huggingface.co/ggerganov/whisper.cpp/tree/main".into(),
            )
        })
}

fn emit_output(ma: &ModularAgent, agent_id: &str, port: &str, value: AgentValue) {
    if let Err(e) = ma.try_send_agent_out(
        agent_id.to_string(),
        AgentContext::new(),
        port.to_string(),
        value,
    ) {
        log::error!("Failed to send output on port '{}': {}", port, e);
    }
}

fn resolve_device(device_id_str: &str) -> Result<cpal::Device, AgentError> {
    let host = cpal::default_host();
    if device_id_str.is_empty() {
        return host
            .default_input_device()
            .ok_or_else(|| AgentError::IoError("No default audio input device available".into()));
    }
    let device_id: cpal::DeviceId = device_id_str
        .parse()
        .map_err(|e| AgentError::InvalidConfig(format!("Invalid device ID '{}': {}", device_id_str, e)))?;
    host.device_by_id(&device_id).ok_or_else(|| {
        let available: Vec<String> = host
            .input_devices()
            .ok()
            .map(|devs| {
                devs.filter_map(|d| {
                    let id = d.id().ok()?;
                    let name = d.description().ok().map(|desc| desc.name().to_string()).unwrap_or_default();
                    Some(format!("{} ({})", id, name))
                })
                .collect()
            })
            .unwrap_or_default();
        AgentError::InvalidConfig(format!(
            "Device with ID '{}' not found. Available: {:?}",
            device_id_str, available
        ))
    })
}

fn process_vad_and_transcribe(
    samples: &[f32],
    vad: &mut EnergyVad,
    whisper_state: &mut whisper_rs::WhisperState,
    language: &Arc<Mutex<String>>,
    min_volume: &Arc<Mutex<f32>>,
    ma: &ModularAgent,
    agent_id: &str,
) {
    if let Some(utterance) = vad.process(samples) {
        let min_vol = *min_volume.lock().unwrap();
        if min_vol > 0.0 {
            let peak = EnergyVad::peak_rms(&utterance, WHISPER_SAMPLE_RATE);
            if peak < min_vol {
                log::debug!(
                    "Utterance discarded: peak_rms {:.4} < min_volume {:.4}",
                    peak,
                    min_vol
                );
                return;
            }
        }
        let lang = language.lock().unwrap().clone();
        match transcribe(whisper_state, &utterance, &lang) {
            Ok(text) if !text.is_empty() => {
                emit_output(ma, agent_id, PORT_TEXT, AgentValue::string(&text));
            }
            Err(e) => {
                log::error!("Whisper inference error: {}", e);
            }
            _ => {}
        }
    }
}

/// Processing thread function.
#[allow(clippy::too_many_arguments)]
fn processing_thread(
    ma: ModularAgent,
    agent_id: String,
    device: cpal::Device,
    model_path: String,
    language: Arc<Mutex<String>>,
    vad_sensitivity: Arc<Mutex<f32>>,
    min_volume: Arc<Mutex<f32>>,
    max_segment_secs: u32,
    cmd_rx: std::sync::mpsc::Receiver<Command>,
) {
    // Emit status
    emit_output(
        &ma,
        &agent_id,
        PORT_STATUS,
        AgentValue::string("recording_started"),
    );

    // Load whisper model (heavy operation, done on this thread)
    let whisper_ctx = match get_or_load_whisper_context(&model_path) {
        Ok(ctx) => ctx,
        Err(e) => {
            emit_output(
                &ma,
                &agent_id,
                PORT_STATUS,
                AgentValue::string(format!("error: {}", e)),
            );
            return;
        }
    };
    let mut whisper_state = match whisper_ctx.create_state() {
        Ok(s) => s,
        Err(e) => {
            emit_output(
                &ma,
                &agent_id,
                PORT_STATUS,
                AgentValue::string(format!("error: failed to create whisper state: {}", e)),
            );
            return;
        }
    };

    // Get device config
    let supported_config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            emit_output(
                &ma,
                &agent_id,
                PORT_STATUS,
                AgentValue::string(format!("error: {}", e)),
            );
            return;
        }
    };
    let device_sample_rate = supported_config.sample_rate();
    let device_channels = supported_config.channels() as usize;

    // Ring buffer: device_sample_rate * channels * 6 seconds
    let ring_size = device_sample_rate as usize * device_channels * 6;
    let (mut producer, mut consumer) = rtrb::RingBuffer::new(ring_size);

    // Error flag for cpal error callback (can't share cmd_tx with the closure)
    let error_flag: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let error_flag_cb = error_flag.clone();

    // Build input stream
    let stream_config = cpal::StreamConfig {
        channels: supported_config.channels(),
        sample_rate: supported_config.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };

    let stream = match device.build_input_stream(
        &stream_config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            // Push samples to ring buffer, silently drop on overflow
            for &sample in data {
                let _ = producer.push(sample);
            }
        },
        move |err| {
            log::error!("Audio input stream error: {}", err);
            if let Ok(mut flag) = error_flag_cb.lock() {
                *flag = Some(format!("{}", err));
            }
        },
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            emit_output(
                &ma,
                &agent_id,
                PORT_STATUS,
                AgentValue::string(format!("error: {}", e)),
            );
            return;
        }
    };

    if let Err(e) = stream.play() {
        emit_output(
            &ma,
            &agent_id,
            PORT_STATUS,
            AgentValue::string(format!("error: {}", e)),
        );
        return;
    }

    // Set up resampler if needed
    let needs_resample = device_sample_rate != WHISPER_SAMPLE_RATE;
    let mut resampler: Option<rubato::SincFixedOut<f64>> = if needs_resample {
        let params = rubato::SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            oversampling_factor: 128,
            interpolation: rubato::SincInterpolationType::Linear,
            window: rubato::WindowFunction::BlackmanHarris2,
        };
        match rubato::SincFixedOut::<f64>::new(
            WHISPER_SAMPLE_RATE as f64 / device_sample_rate as f64,
            2.0,
            params,
            160, // output chunk size: 10ms at 16kHz
            1,   // mono
        ) {
            Ok(r) => Some(r),
            Err(e) => {
                emit_output(
                    &ma,
                    &agent_id,
                    PORT_STATUS,
                    AgentValue::string(format!("error: resampler init failed: {}", e)),
                );
                return;
            }
        }
    } else {
        None
    };

    let initial_sensitivity = *vad_sensitivity.lock().unwrap();
    let mut vad = EnergyVad::new(WHISPER_SAMPLE_RATE, initial_sensitivity, max_segment_secs);
    let mut paused = false;
    // Read ~10ms of interleaved samples per iteration
    let chunk_size = (device_sample_rate as usize * device_channels) / 100;
    // Buffer for accumulating mono samples before resampling
    let mut mono_buf: Vec<f32> = Vec::new();

    loop {
        // 1. Check commands (non-blocking)
        match cmd_rx.try_recv() {
            Ok(Command::Shutdown) => break,
            Ok(Command::Pause) => {
                paused = true;
                continue;
            }
            Ok(Command::Resume) => {
                paused = false;
            }
            Err(_) => {}
        }

        // Check error flag from cpal callback
        if let Ok(mut flag) = error_flag.lock()
            && let Some(err) = flag.take()
        {
            emit_output(
                &ma,
                &agent_id,
                PORT_STATUS,
                AgentValue::string(format!("error: {}", err)),
            );
            break;
        }

        if paused {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        // Update VAD sensitivity from config
        if let Ok(t) = vad_sensitivity.lock() {
            vad.set_threshold(*t);
        }

        // 2. Read from ring buffer
        let available = consumer.slots();
        if available < chunk_size {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        let read_count = chunk_size.min(available);
        let mut interleaved = vec![0.0f32; read_count];
        if let Ok(chunk) = consumer.read_chunk(read_count) {
            let (first, second) = chunk.as_slices();
            interleaved[..first.len()].copy_from_slice(first);
            if !second.is_empty() {
                interleaved[first.len()..first.len() + second.len()].copy_from_slice(second);
            }
            chunk.commit_all();
        } else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        // 3. Multi-channel to mono
        if device_channels > 1 {
            for ch in interleaved.chunks(device_channels) {
                mono_buf.push(ch.iter().sum::<f32>() / device_channels as f32);
            }
        } else {
            mono_buf.extend_from_slice(&interleaved);
        }

        // 4. Resample to 16kHz (or bypass) and feed VAD
        if let Some(ref mut resampler) = resampler {
            use rubato::Resampler;
            // Feed the resampler whenever we have enough input samples
            while mono_buf.len() >= resampler.input_frames_next() {
                let needed = resampler.input_frames_next();
                let input_f64: Vec<f64> = mono_buf[..needed].iter().map(|&s| s as f64).collect();
                mono_buf.drain(..needed);
                match resampler.process(&[input_f64], None) {
                    Ok(output) => {
                        if !output.is_empty() && !output[0].is_empty() {
                            let samples_16k: Vec<f32> =
                                output[0].iter().map(|&s| s as f32).collect();
                            process_vad_and_transcribe(
                                &samples_16k,
                                &mut vad,
                                &mut whisper_state,
                                &language,
                                &min_volume,
                                &ma,
                                &agent_id,
                            );
                        }
                    }
                    Err(e) => {
                        log::error!("Resample error: {}", e);
                    }
                }
            }
        } else {
            // No resampling needed — feed mono samples directly to VAD
            let samples = std::mem::take(&mut mono_buf);
            process_vad_and_transcribe(
                &samples,
                &mut vad,
                &mut whisper_state,
                &language,
                &min_volume,
                &ma,
                &agent_id,
            );
        }
    }

    // Stream is dropped here, stopping the cpal callback
    drop(stream);
    emit_output(
        &ma,
        &agent_id,
        PORT_STATUS,
        AgentValue::string("recording_stopped"),
    );
}

fn transcribe(
    state: &mut whisper_rs::WhisperState,
    samples: &[f32],
    language: &str,
) -> Result<String, AgentError> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(language));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_single_segment(true);
    params.set_no_context(true);
    params.set_n_threads(2);

    state
        .full(params, samples)
        .map_err(|e| AgentError::IoError(format!("Whisper inference failed: {}", e)))?;

    let n_segments = state.full_n_segments();
    let mut text = String::new();
    for i in 0..n_segments {
        if let Some(segment) = state.get_segment(i)
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

/// Captures microphone audio, detects speech via VAD,
/// and transcribes with Whisper (whisper.cpp).
#[modular_agent(
    title = "Mic Transcribe",
    category = CATEGORY,
    outputs = [PORT_TEXT, PORT_STATUS],
    boolean_config(name = CONFIG_ENABLED, default = true, description = "Enable/disable mic capture"),
    string_config(name = CONFIG_DEVICE, description = "Audio input device ID (empty = default)"),
    string_config(name = CONFIG_LANGUAGE, default = "ja", detail, description = "Language code for transcription"),
    number_config(name = CONFIG_VAD_SENSITIVITY, default = 0.01, detail, description = "VAD sensitivity (RMS threshold, lower = more sensitive)"),
    number_config(name = CONFIG_MIN_VOLUME, default = 0.0, detail, description = "Minimum peak volume (RMS) to send to Whisper. Utterances below this are discarded. 0 = disabled"),
    integer_config(name = CONFIG_MAX_SEGMENT_DURATION, default = 25, detail, description = "Max segment duration in seconds (Whisper 30s limit)"),
    string_global_config(name = CONFIG_MODEL_PATH, description = "Path to Whisper GGML model file (e.g. ggml-medium.bin)"),
    hint(color = 5, width = 1, height = 1),
)]
struct MicTranscribeAgent {
    data: AgentData,
    cmd_tx: Mutex<Option<std::sync::mpsc::Sender<Command>>>,
    thread_handle: Mutex<Option<JoinHandle<()>>>,
    shared_vad_sensitivity: Arc<Mutex<f32>>,
    shared_min_volume: Arc<Mutex<f32>>,
    shared_language: Arc<Mutex<String>>,
}

#[async_trait]
impl AsAgent for MicTranscribeAgent {
    fn new(ma: ModularAgent, id: String, spec: AgentSpec) -> Result<Self, AgentError> {
        Ok(Self {
            data: AgentData::new(ma, id, spec),
            cmd_tx: Mutex::new(None),
            thread_handle: Mutex::new(None),
            shared_vad_sensitivity: Arc::new(Mutex::new(0.01)),
            shared_min_volume: Arc::new(Mutex::new(0.0)),
            shared_language: Arc::new(Mutex::new("ja".to_string())),
        })
    }

    async fn start(&mut self) -> Result<(), AgentError> {
        let config = self.configs()?;
        let enabled = config.get_bool_or(CONFIG_ENABLED, true);
        if !enabled {
            return Ok(());
        }

        // Validate model path (file existence check only, actual loading on thread)
        let model_path = get_model_path(self.ma())?;
        if !std::path::Path::new(&model_path).exists() {
            return Err(AgentError::InvalidConfig(format!(
                "Whisper model file not found: {}. Download from https://huggingface.co/ggerganov/whisper.cpp/tree/main",
                model_path
            )));
        }

        let device_id = config.get_string_or_default(CONFIG_DEVICE);
        let language = config.get_string_or(CONFIG_LANGUAGE, "ja");
        let sensitivity = config.get_number_or(CONFIG_VAD_SENSITIVITY, 0.01) as f32;
        let min_vol = (config.get_number_or(CONFIG_MIN_VOLUME, 0.0) as f32).clamp(0.0, 1.0);
        let max_seg = config.get_integer_or(CONFIG_MAX_SEGMENT_DURATION, 25) as u32;

        // Update shared state
        *self.shared_vad_sensitivity.lock().unwrap() = sensitivity;
        *self.shared_min_volume.lock().unwrap() = min_vol;
        *self.shared_language.lock().unwrap() = language.clone();

        // Resolve device on main thread (Device is Send)
        let device = resolve_device(&device_id)?;

        let ma = self.ma().clone();
        let agent_id = self.id().to_string();
        let shared_language = self.shared_language.clone();
        let shared_vad_sensitivity = self.shared_vad_sensitivity.clone();
        let shared_min_volume = self.shared_min_volume.clone();

        let (tx, rx) = std::sync::mpsc::channel();

        let handle = std::thread::Builder::new()
            .name(format!("mic-transcribe-{}", agent_id))
            .spawn(move || {
                processing_thread(
                    ma,
                    agent_id,
                    device,
                    model_path,
                    shared_language,
                    shared_vad_sensitivity,
                    shared_min_volume,
                    max_seg,
                    rx,
                );
            })
            .map_err(|e| {
                AgentError::IoError(format!("Failed to spawn processing thread: {}", e))
            })?;

        *self.cmd_tx.lock().unwrap() = Some(tx);
        *self.thread_handle.lock().unwrap() = Some(handle);

        Ok(())
    }

    async fn stop(&mut self) -> Result<(), AgentError> {
        let tx = self.cmd_tx.lock().unwrap().take();
        if let Some(tx) = tx {
            let _ = tx.send(Command::Shutdown);
        }
        let handle = self.thread_handle.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        Ok(())
    }

    fn configs_changed(&mut self) -> Result<(), AgentError> {
        if *self.status() != AgentStatus::Start {
            return Ok(());
        }

        let config = self.configs()?;

        // Update shared VAD sensitivity
        let sensitivity = config.get_number_or(CONFIG_VAD_SENSITIVITY, 0.01) as f32;
        *self.shared_vad_sensitivity.lock().unwrap() = sensitivity;

        // Update shared min_volume
        let min_vol = (config.get_number_or(CONFIG_MIN_VOLUME, 0.0) as f32).clamp(0.0, 1.0);
        *self.shared_min_volume.lock().unwrap() = min_vol;

        // Update shared language
        let language = config.get_string_or(CONFIG_LANGUAGE, "ja");
        *self.shared_language.lock().unwrap() = language;

        // Handle enabled toggle
        let enabled = config.get_bool_or(CONFIG_ENABLED, true);
        let has_thread = self.cmd_tx.lock().unwrap().is_some();

        if !enabled && has_thread {
            // Pause
            if let Some(ref tx) = *self.cmd_tx.lock().unwrap() {
                let _ = tx.send(Command::Pause);
            }
        } else if enabled && has_thread {
            // Resume
            if let Some(ref tx) = *self.cmd_tx.lock().unwrap() {
                let _ = tx.send(Command::Resume);
            }
        }

        // Device change requires thread restart (handled by user stopping/starting)

        Ok(())
    }

    async fn process(
        &mut self,
        _ctx: AgentContext,
        _port: String,
        _value: AgentValue,
    ) -> Result<(), AgentError> {
        Ok(()) // no-op: source agent
    }
}
