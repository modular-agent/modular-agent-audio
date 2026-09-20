use std::collections::VecDeque;

use modular_agent_core::{Error, Result};
use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};

use crate::vad::Vad;

const SAMPLE_RATE: usize = 16000;
/// Silero VAD's fixed frame size at 16 kHz.
const WINDOW: usize = 512;
const MIN_SILENCE_SECS: f32 = 0.35;
const MIN_SPEECH_SECS: f32 = 0.25;
/// Silero places a segment's start where the speech probability crossed the
/// threshold, which is after the first soft phonemes. Every utterance is
/// extended backwards by this much so those are transcribed too.
const PRE_ROLL_SECS: f32 = 0.8;
/// Internal segment buffer of the detector.
const BUFFER_SECS: f32 = 30.0;
/// Extra history kept beyond the longest possible utterance, so a segment's
/// pre-roll is still available when the segment is finalized.
const HISTORY_MARGIN_SECS: usize = 2;
/// Trimming the history every window would be quadratic; trim in batches.
const HISTORY_TRIM_SLACK: usize = SAMPLE_RATE * 4;

const PRE_ROLL: usize = (PRE_ROLL_SECS * SAMPLE_RATE as f32) as usize;
const MIN_SPEECH: usize = (MIN_SPEECH_SECS * SAMPLE_RATE as f32) as usize;

/// Silero VAD via sherpa-onnx. Utterance boundaries come from the model; the
/// recent input is kept here because the C API neither exposes the in-progress
/// segment nor pads finished segments.
pub(crate) struct SileroVad {
    detector: VoiceActivityDetector,
    /// Samples not yet forming a full window.
    carry: Vec<f32>,
    /// Recent input; `history[0]` is sample `history_start` of the stream.
    history: Vec<f32>,
    history_start: usize,
    history_cap: usize,
    /// Samples fed to the detector so far (stream position).
    fed: usize,
    speaking: bool,
    /// Stream position where the in-progress utterance (with pre-roll) begins.
    speech_start: usize,
    pending: VecDeque<Vec<f32>>,
}

impl SileroVad {
    pub(crate) fn new(model_path: &str, threshold: f32, max_speech_secs: u32) -> Result<Self> {
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(model_path.to_string()),
                threshold,
                min_silence_duration: MIN_SILENCE_SECS,
                min_speech_duration: MIN_SPEECH_SECS,
                window_size: WINDOW as i32,
                max_speech_duration: max_speech_secs as f32,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            provider: Some("cpu".into()),
            ..Default::default()
        };
        let detector = VoiceActivityDetector::create(&config, BUFFER_SECS).ok_or_else(|| {
            Error::IoError(format!("Failed to create Silero VAD from '{}'", model_path))
        })?;
        let history_cap = (max_speech_secs as usize + HISTORY_MARGIN_SECS) * SAMPLE_RATE;
        Ok(Self {
            detector,
            carry: Vec::with_capacity(WINDOW * 2),
            history: Vec::with_capacity(history_cap + HISTORY_TRIM_SLACK),
            history_start: 0,
            history_cap,
            fed: 0,
            speaking: false,
            speech_start: 0,
            pending: VecDeque::new(),
        })
    }

    fn feed_window(&mut self, window: &[f32]) {
        self.detector.accept_waveform(window);
        self.history.extend_from_slice(window);
        self.fed += window.len();
        if self.history.len() > self.history_cap + HISTORY_TRIM_SLACK {
            let excess = self.history.len() - self.history_cap;
            self.history.drain(..excess);
            self.history_start += excess;
        }

        let speaking = self.detector.detected();
        if speaking && !self.speaking {
            // detected() lags the onset by min_speech; back up past it plus the pre-roll.
            self.speech_start = self
                .fed
                .saturating_sub(window.len() + MIN_SPEECH + PRE_ROLL);
        }
        self.speaking = speaking;

        while let Some(segment) = self.detector.front() {
            let start = segment.start().max(0) as usize;
            let pre_from = start.saturating_sub(PRE_ROLL).max(self.history_start);
            let pre_to = start
                .max(pre_from)
                .min(self.history_start + self.history.len());
            let mut utterance = Vec::with_capacity(pre_to - pre_from + segment.samples().len());
            utterance.extend_from_slice(
                &self.history[pre_from - self.history_start..pre_to - self.history_start],
            );
            utterance.extend_from_slice(segment.samples());
            self.pending.push_back(utterance);
            self.detector.pop();
        }
    }
}

impl Vad for SileroVad {
    fn process(&mut self, samples: &[f32]) -> Option<Vec<f32>> {
        self.carry.extend_from_slice(samples);
        let mut offset = 0;
        while self.carry.len() - offset >= WINDOW {
            let window: Vec<f32> = self.carry[offset..offset + WINDOW].to_vec();
            self.feed_window(&window);
            offset += WINDOW;
        }
        self.carry.drain(..offset);
        self.pending.pop_front()
    }

    fn is_speaking(&self) -> bool {
        self.speaking
    }

    fn current_speech(&self) -> &[f32] {
        if !self.speaking {
            return &[];
        }
        let from = self.speech_start.max(self.history_start) - self.history_start;
        &self.history[from..]
    }

    fn set_energy_threshold(&mut self, _threshold: f32) {}
}
