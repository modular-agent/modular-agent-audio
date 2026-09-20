use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use modular_agent_core::{
    AsModule, Error, ModularAgent, Module, ModuleContext, ModuleData, ModuleSpec, ModuleStatus,
    Result, Value, async_trait, modular_agent,
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::vad::EnergyVad;

const CATEGORY: &str = "Audio";
const WHISPER_SAMPLE_RATE: u32 = 16000;

const PORT_TEXT: &str = "text";
const PORT_PARTIAL: &str = "partial";
const PORT_STATUS: &str = "status";

const CONFIG_ENABLED: &str = "enabled";
const CONFIG_DEVICE: &str = "device";
const CONFIG_LANGUAGE: &str = "language";
const CONFIG_VAD_SENSITIVITY: &str = "vad_sensitivity";
const CONFIG_MIN_VOLUME: &str = "min_volume";
const CONFIG_MAX_SEGMENT_DURATION: &str = "max_segment_duration";
const CONFIG_SILENCE_DURATION_MS: &str = "silence_duration_ms";
const CONFIG_PARTIAL_INTERVAL: &str = "partial_interval";
const CONFIG_MODEL_PATH: &str = "model_path";

/// `device` value that selects the default output device as a WASAPI loopback source.
const DEVICE_LOOPBACK: &str = "loopback";

/// Seconds of the in-progress utterance re-decoded for each partial result.
const PARTIAL_WINDOW_SECS: usize = 8;
/// Inference jobs that may wait for the worker before new utterances are dropped.
const INFER_QUEUE_DEPTH: usize = 3;

enum Command {
    Pause,
    Resume,
    Shutdown,
}

enum InferJob {
    Final(Vec<f32>),
    Partial(Vec<f32>),
}

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

fn get_model_path(ma: &ModularAgent) -> Result<String> {
    ma.get_global_configs(MicTranscribeModule::DEF_NAME)
        .and_then(|cfg| cfg.get_string(CONFIG_MODEL_PATH).ok())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            Error::InvalidConfig(
                "Whisper model path not set. Download from https://huggingface.co/ggerganov/whisper.cpp/tree/main".into(),
            )
        })
}

fn emit_output(ma: &ModularAgent, module_id: &str, port: &str, value: Value) {
    if let Err(e) = ma.try_send_module_out(
        module_id.to_string(),
        ModuleContext::new(),
        port.to_string(),
        value,
    ) {
        log::error!("Failed to send output on port '{}': {}", port, e);
    }
}

fn emit_status(ma: &ModularAgent, module_id: &str, status: impl Into<String>) {
    emit_output(ma, module_id, PORT_STATUS, Value::string(status.into()));
}

fn resolve_device(device_id_str: &str) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if device_id_str.is_empty() {
        return host
            .default_input_device()
            .ok_or_else(|| Error::IoError("No default audio input device available".into()));
    }
    if device_id_str == DEVICE_LOOPBACK {
        if !cfg!(target_os = "windows") {
            return Err(Error::InvalidConfig(
                "Loopback capture is only supported on Windows (WASAPI)".into(),
            ));
        }
        return host
            .default_output_device()
            .ok_or_else(|| Error::IoError("No default audio output device available".into()));
    }
    let device_id: cpal::DeviceId = device_id_str.parse().map_err(|e| {
        Error::InvalidConfig(format!("Invalid device ID '{}': {}", device_id_str, e))
    })?;
    host.device_by_id(&device_id).ok_or_else(|| {
        let available: Vec<String> = host
            .input_devices()
            .ok()
            .map(|devs| {
                devs.filter_map(|d| {
                    let id = d.id().ok()?;
                    let name = d
                        .description()
                        .ok()
                        .map(|desc| crate::device_list::display_name(&desc))
                        .unwrap_or_default();
                    Some(format!("{} ({})", id, name))
                })
                .collect()
            })
            .unwrap_or_default();
        Error::InvalidConfig(format!(
            "Device with ID '{}' not found. Available: {:?}",
            device_id_str, available
        ))
    })
}

/// Segments 16 kHz mono audio into utterances and hands them to the inference worker.
struct SpeechPipeline {
    ma: ModularAgent,
    module_id: String,
    vad: EnergyVad,
    job_tx: SyncSender<InferJob>,
    min_volume: Arc<Mutex<f32>>,
    /// 0 disables partial results.
    partial_interval_samples: usize,
    samples_since_partial: usize,
}

impl SpeechPipeline {
    fn feed(&mut self, samples_16k: &[f32]) {
        if let Some(utterance) = self.vad.process(samples_16k) {
            self.samples_since_partial = 0;
            if self.passes_min_volume(&utterance) {
                self.submit(InferJob::Final(utterance));
            }
            return;
        }

        if self.partial_interval_samples == 0 || !self.vad.is_speaking() {
            self.samples_since_partial = 0;
            return;
        }
        self.samples_since_partial += samples_16k.len();
        if self.samples_since_partial >= self.partial_interval_samples {
            self.samples_since_partial = 0;
            let speech = self.vad.current_speech();
            let window = speech
                .len()
                .min(PARTIAL_WINDOW_SECS * WHISPER_SAMPLE_RATE as usize);
            self.submit(InferJob::Partial(speech[speech.len() - window..].to_vec()));
        }
    }

    fn passes_min_volume(&self, utterance: &[f32]) -> bool {
        let min_vol = *self.min_volume.lock().unwrap();
        if min_vol <= 0.0 {
            return true;
        }
        let peak = EnergyVad::peak_rms(utterance, WHISPER_SAMPLE_RATE);
        if peak < min_vol {
            log::debug!(
                "Utterance discarded: peak_rms {:.4} < min_volume {:.4}",
                peak,
                min_vol
            );
            return false;
        }
        true
    }

    /// Never blocks: the audio path must keep draining the ring buffer even
    /// when inference falls behind, so a full queue drops the job instead.
    fn submit(&self, job: InferJob) {
        let is_final = matches!(job, InferJob::Final(_));
        match self.job_tx.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) if is_final => {
                log::warn!("Transcription backlog full; utterance dropped");
                emit_status(&self.ma, &self.module_id, "dropped: transcription backlog");
            }
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {
                log::error!("Inference worker has exited; utterance dropped");
            }
        }
    }
}

/// A partial is only worth decoding while nothing newer is queued behind it.
/// Skips ahead to the newest job, stopping at a Final so none is lost.
fn skip_stale_partials(job_rx: &Receiver<InferJob>, mut job: InferJob) -> InferJob {
    while matches!(job, InferJob::Partial(_)) {
        match job_rx.try_recv() {
            Ok(next) => job = next,
            Err(_) => break,
        }
    }
    job
}

fn whisper_n_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(2)
        .clamp(2, 8) as i32
}

/// Inference worker: owns the Whisper state and decodes jobs from the processing thread.
/// Reports the model load result through `ready_tx` before entering the loop.
fn infer_thread(
    ma: ModularAgent,
    module_id: String,
    model_path: String,
    language: Arc<Mutex<String>>,
    stopping: Arc<AtomicBool>,
    ready_tx: SyncSender<Result<()>>,
    job_rx: Receiver<InferJob>,
) {
    let mut whisper_state = match get_or_load_whisper_context(&model_path).and_then(|ctx| {
        ctx.create_state()
            .map_err(|e| Error::IoError(format!("Failed to create whisper state: {}", e)))
    }) {
        Ok(state) => state,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    let _ = ready_tx.send(Ok(()));
    let n_threads = whisper_n_threads();

    while let Ok(job) = job_rx.recv() {
        if stopping.load(Ordering::Relaxed) {
            break;
        }
        let (port, samples) = match skip_stale_partials(&job_rx, job) {
            InferJob::Final(samples) => (PORT_TEXT, samples),
            InferJob::Partial(samples) => (PORT_PARTIAL, samples),
        };
        let lang = language.lock().unwrap().clone();
        match transcribe(&mut whisper_state, &samples, &lang, n_threads) {
            Ok(text) if !text.is_empty() => {
                emit_output(&ma, &module_id, port, Value::string(&text));
            }
            Err(e) => {
                log::error!("Whisper inference error: {}", e);
            }
            _ => {}
        }
    }
}

struct CaptureParams {
    device: cpal::Device,
    vad_sensitivity: Arc<Mutex<f32>>,
    min_volume: Arc<Mutex<f32>>,
    max_segment_secs: u32,
    silence_duration_ms: u32,
    partial_interval_secs: f64,
}

/// Processing thread: spawns the inference worker, then captures until told to stop.
fn processing_thread(
    ma: ModularAgent,
    module_id: String,
    model_path: String,
    language: Arc<Mutex<String>>,
    params: CaptureParams,
    cmd_rx: Receiver<Command>,
) {
    emit_status(&ma, &module_id, "recording_started");

    let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<InferJob>(INFER_QUEUE_DEPTH);
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<()>>(1);
    let stopping = Arc::new(AtomicBool::new(false));

    let worker = {
        let ma = ma.clone();
        let module_id = module_id.clone();
        let stopping = stopping.clone();
        std::thread::Builder::new()
            .name(format!("mic-transcribe-infer-{}", module_id))
            .spawn(move || {
                infer_thread(
                    ma, module_id, model_path, language, stopping, ready_tx, job_rx,
                )
            })
    };
    let worker = match worker {
        Ok(handle) => handle,
        Err(e) => {
            emit_status(
                &ma,
                &module_id,
                format!("error: failed to spawn inference thread: {}", e),
            );
            return;
        }
    };

    // `job_tx` moves into the capture loop (or is dropped with the unused
    // closure), so the worker's queue disconnects before it is joined below.
    let result = ready_rx
        .recv()
        .unwrap_or_else(|_| {
            Err(Error::IoError(
                "Inference worker exited before loading the model".into(),
            ))
        })
        .and_then(|()| capture_loop(&ma, &module_id, params, job_tx, &cmd_rx));

    stopping.store(true, Ordering::Relaxed);
    let _ = worker.join();

    match result {
        Ok(()) => emit_status(&ma, &module_id, "recording_stopped"),
        Err(e) => emit_status(&ma, &module_id, format!("error: {}", e)),
    }
}

/// Captures from the device and feeds the VAD until `Command::Shutdown` or a stream error.
fn capture_loop(
    ma: &ModularAgent,
    module_id: &str,
    params: CaptureParams,
    job_tx: SyncSender<InferJob>,
    cmd_rx: &Receiver<Command>,
) -> Result<()> {
    let CaptureParams {
        device,
        vad_sensitivity,
        min_volume,
        max_segment_secs,
        silence_duration_ms,
        partial_interval_secs,
    } = params;

    // A render endpoint (WASAPI loopback) rejects default_input_config(), but
    // build_input_stream() on it captures the mix being played through it.
    let config_result = if device.supports_input() {
        device.default_input_config()
    } else {
        device.default_output_config()
    };
    let supported_config = config_result.map_err(|e| Error::IoError(e.to_string()))?;
    let device_sample_rate = supported_config.sample_rate();
    let device_channels = supported_config.channels() as usize;

    // Ring buffer: device_sample_rate * channels * 6 seconds
    let ring_size = device_sample_rate as usize * device_channels * 6;
    let (mut producer, mut consumer) = rtrb::RingBuffer::new(ring_size);

    // Error flag for cpal error callback (can't share cmd_tx with the closure)
    let error_flag: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let error_flag_cb = error_flag.clone();
    // Samples the callback could not push because the ring buffer was full
    let overrun = Arc::new(AtomicUsize::new(0));
    let overrun_cb = overrun.clone();

    let stream_config = cpal::StreamConfig {
        channels: supported_config.channels(),
        sample_rate: supported_config.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };

    let stream = device
        .build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut dropped = 0;
                for &sample in data {
                    if producer.push(sample).is_err() {
                        dropped += 1;
                    }
                }
                if dropped > 0 {
                    overrun_cb.fetch_add(dropped, Ordering::Relaxed);
                }
            },
            move |err| {
                log::error!("Audio input stream error: {}", err);
                if let Ok(mut flag) = error_flag_cb.lock() {
                    *flag = Some(format!("{}", err));
                }
            },
            None,
        )
        .map_err(|e| Error::IoError(e.to_string()))?;

    stream.play().map_err(|e| Error::IoError(e.to_string()))?;

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
        let resampler = rubato::SincFixedOut::<f64>::new(
            WHISPER_SAMPLE_RATE as f64 / device_sample_rate as f64,
            2.0,
            params,
            160, // output chunk size: 10ms at 16kHz
            1,   // mono
        )
        .map_err(|e| Error::IoError(format!("resampler init failed: {}", e)))?;
        Some(resampler)
    } else {
        None
    };

    let initial_sensitivity = *vad_sensitivity.lock().unwrap();
    let mut pipeline = SpeechPipeline {
        ma: ma.clone(),
        module_id: module_id.to_string(),
        vad: EnergyVad::new(
            WHISPER_SAMPLE_RATE,
            initial_sensitivity,
            max_segment_secs,
            silence_duration_ms,
        ),
        job_tx,
        min_volume,
        partial_interval_samples: (partial_interval_secs.max(0.0) * WHISPER_SAMPLE_RATE as f64)
            as usize,
        samples_since_partial: 0,
    };
    let mut paused = false;
    let mut in_overrun = false;
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
            return Err(Error::IoError(err));
        }

        let dropped = overrun.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            log::warn!("Input overrun: {} samples dropped", dropped);
            if !in_overrun {
                emit_status(ma, module_id, "overrun: input samples dropped");
            }
        }
        in_overrun = dropped > 0;

        if paused {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }

        // Update VAD sensitivity from config
        if let Ok(t) = vad_sensitivity.lock() {
            pipeline.vad.set_threshold(*t);
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
                            pipeline.feed(&samples_16k);
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
            pipeline.feed(&samples);
        }
    }

    // Stream is dropped here, stopping the cpal callback
    drop(stream);
    Ok(())
}

fn transcribe(
    state: &mut whisper_rs::WhisperState,
    samples: &[f32],
    language: &str,
    n_threads: i32,
) -> Result<String> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(language));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_single_segment(true);
    params.set_no_context(true);
    params.set_n_threads(n_threads);

    state
        .full(params, samples)
        .map_err(|e| Error::IoError(format!("Whisper inference failed: {}", e)))?;

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
    outputs = [PORT_TEXT, PORT_PARTIAL, PORT_STATUS],
    boolean_config(name = CONFIG_ENABLED, default = true, description = "Enable/disable mic capture"),
    string_config(name = CONFIG_DEVICE, description = "Audio input device ID (empty = default mic, \"loopback\" = default output on Windows)"),
    string_config(name = CONFIG_LANGUAGE, default = "ja", detail, description = "Language code for transcription"),
    number_config(name = CONFIG_VAD_SENSITIVITY, default = 0.01, detail, description = "VAD sensitivity (RMS threshold, lower = more sensitive)"),
    number_config(name = CONFIG_MIN_VOLUME, default = 0.0, detail, description = "Minimum peak volume (RMS) to send to Whisper. Utterances below this are discarded. 0 = disabled"),
    integer_config(name = CONFIG_MAX_SEGMENT_DURATION, default = 25, detail, description = "Max segment duration in seconds (Whisper 30s limit)"),
    integer_config(name = CONFIG_SILENCE_DURATION_MS, default = 800, detail, description = "Trailing silence in milliseconds that ends an utterance (lower = faster finals, more mid-sentence splits)"),
    number_config(name = CONFIG_PARTIAL_INTERVAL, default = 0.0, detail, description = "Seconds of speech between partial results while an utterance is in progress. 0 = disabled"),
    string_global_config(name = CONFIG_MODEL_PATH, description = "Path to Whisper GGML model file (e.g. ggml-medium.bin)"),
    hint(color = 5, width = 1, height = 1),
)]
struct MicTranscribeModule {
    data: ModuleData,
    cmd_tx: Mutex<Option<std::sync::mpsc::Sender<Command>>>,
    thread_handle: Mutex<Option<JoinHandle<()>>>,
    shared_vad_sensitivity: Arc<Mutex<f32>>,
    shared_min_volume: Arc<Mutex<f32>>,
    shared_language: Arc<Mutex<String>>,
}

#[async_trait]
impl AsModule for MicTranscribeModule {
    fn new(ma: ModularAgent, id: String, spec: ModuleSpec) -> Result<Self> {
        Ok(Self {
            data: ModuleData::new(ma, id, spec),
            cmd_tx: Mutex::new(None),
            thread_handle: Mutex::new(None),
            shared_vad_sensitivity: Arc::new(Mutex::new(0.01)),
            shared_min_volume: Arc::new(Mutex::new(0.0)),
            shared_language: Arc::new(Mutex::new("ja".to_string())),
        })
    }

    async fn start(&mut self) -> Result<()> {
        let config = self.configs()?;
        let enabled = config.get_bool_or(CONFIG_ENABLED, true);
        if !enabled {
            return Ok(());
        }

        // Validate model path (file existence check only, actual loading on thread)
        let model_path = get_model_path(self.ma())?;
        if !std::path::Path::new(&model_path).exists() {
            return Err(Error::InvalidConfig(format!(
                "Whisper model file not found: {}. Download from https://huggingface.co/ggerganov/whisper.cpp/tree/main",
                model_path
            )));
        }

        let device_id = config.get_string_or_default(CONFIG_DEVICE);
        let language = config.get_string_or(CONFIG_LANGUAGE, "ja");
        let sensitivity = config.get_number_or(CONFIG_VAD_SENSITIVITY, 0.01) as f32;
        let min_vol = (config.get_number_or(CONFIG_MIN_VOLUME, 0.0) as f32).clamp(0.0, 1.0);
        let max_seg = config.get_integer_or(CONFIG_MAX_SEGMENT_DURATION, 25) as u32;
        let silence_ms = config
            .get_integer_or(CONFIG_SILENCE_DURATION_MS, 800)
            .max(0) as u32;
        let partial_interval = config.get_number_or(CONFIG_PARTIAL_INTERVAL, 0.0);

        // Update shared state
        *self.shared_vad_sensitivity.lock().unwrap() = sensitivity;
        *self.shared_min_volume.lock().unwrap() = min_vol;
        *self.shared_language.lock().unwrap() = language.clone();

        // Resolve device on main thread (Device is Send)
        let device = resolve_device(&device_id)?;

        let ma = self.ma().clone();
        let module_id = self.id().to_string();
        let shared_language = self.shared_language.clone();
        let params = CaptureParams {
            device,
            vad_sensitivity: self.shared_vad_sensitivity.clone(),
            min_volume: self.shared_min_volume.clone(),
            max_segment_secs: max_seg,
            silence_duration_ms: silence_ms,
            partial_interval_secs: partial_interval,
        };

        let (tx, rx) = std::sync::mpsc::channel();

        let handle = std::thread::Builder::new()
            .name(format!("mic-transcribe-{}", module_id))
            .spawn(move || {
                processing_thread(ma, module_id, model_path, shared_language, params, rx);
            })
            .map_err(|e| Error::IoError(format!("Failed to spawn processing thread: {}", e)))?;

        *self.cmd_tx.lock().unwrap() = Some(tx);
        *self.thread_handle.lock().unwrap() = Some(handle);

        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
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

    fn configs_changed(&mut self) -> Result<()> {
        if *self.status() != ModuleStatus::Start {
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

    async fn process(&mut self, _ctx: ModuleContext, _port: String, _value: Value) -> Result<()> {
        Ok(()) // no-op: source module
    }
}
