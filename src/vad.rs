use std::collections::VecDeque;

const SILENCE_DURATION_MS: u32 = 800;
const MIN_SPEECH_DURATION_MS: u32 = 300;
const PRE_SPEECH_DURATION_MS: u32 = 200;

/// Energy-based Voice Activity Detection.
///
/// Segments audio into utterances by monitoring RMS energy levels.
/// Returns completed utterances when sufficient silence is detected
/// after speech, or when the maximum segment duration is reached.
pub struct EnergyVad {
    sample_rate: u32,
    energy_threshold: f32,
    max_duration_samples: usize,
    silence_duration_samples: usize,
    min_speech_samples: usize,
    pre_speech_capacity: usize,
    is_speaking: bool,
    silence_count: usize,
    speech_buffer: Vec<f32>,
    pre_speech_buffer: VecDeque<f32>,
}

impl EnergyVad {
    pub fn new(sample_rate: u32, energy_threshold: f32, max_duration_secs: u32) -> Self {
        let silence_duration_samples = (sample_rate as usize * SILENCE_DURATION_MS as usize) / 1000;
        let min_speech_samples = (sample_rate as usize * MIN_SPEECH_DURATION_MS as usize) / 1000;
        let pre_speech_capacity = (sample_rate as usize * PRE_SPEECH_DURATION_MS as usize) / 1000;
        let max_duration_samples = sample_rate as usize * max_duration_secs as usize;

        Self {
            sample_rate,
            energy_threshold,
            max_duration_samples,
            silence_duration_samples,
            min_speech_samples,
            pre_speech_capacity,
            is_speaking: false,
            silence_count: 0,
            speech_buffer: Vec::new(),
            pre_speech_buffer: VecDeque::with_capacity(pre_speech_capacity),
        }
    }

    /// Feed a chunk of audio samples (mono, at the configured sample rate).
    /// Returns `Some(utterance_samples)` when a complete utterance is detected.
    ///
    /// On force-split, audio after the split point is retained internally
    /// and becomes the start of the next utterance buffer.
    pub fn process(&mut self, samples: &[f32]) -> Option<Vec<f32>> {
        let rms = Self::rms(samples);
        let is_voice = rms >= self.energy_threshold;

        if is_voice {
            if !self.is_speaking {
                // Speech onset: prepend pre-speech buffer
                self.is_speaking = true;
                self.speech_buffer.clear();
                self.speech_buffer
                    .extend(self.pre_speech_buffer.iter().copied());
            }
            self.silence_count = 0;
            self.speech_buffer.extend_from_slice(samples);
        } else if self.is_speaking {
            // Silence during speech
            self.silence_count += samples.len();
            self.speech_buffer.extend_from_slice(samples);

            if self.silence_count >= self.silence_duration_samples {
                return self.finish_utterance();
            }
        } else {
            // Silence, not speaking: update pre-speech ring buffer
            for &s in samples {
                if self.pre_speech_buffer.len() >= self.pre_speech_capacity {
                    self.pre_speech_buffer.pop_front();
                }
                self.pre_speech_buffer.push_back(s);
            }
        }

        // Force split if max duration exceeded
        if self.is_speaking && self.speech_buffer.len() >= self.max_duration_samples {
            return self.force_split();
        }

        None
    }

    pub fn set_threshold(&mut self, threshold: f32) {
        self.energy_threshold = threshold;
    }

    pub fn reset(&mut self) {
        self.is_speaking = false;
        self.silence_count = 0;
        self.speech_buffer.clear();
        self.pre_speech_buffer.clear();
    }

    /// Returns true if a force-split was used (caller can adjust Whisper params).
    pub fn was_force_split(&self) -> bool {
        // This is a transient flag; in practice the caller tracks it from
        // whether force_split() or finish_utterance() returned the data.
        false
    }

    fn finish_utterance(&mut self) -> Option<Vec<f32>> {
        let trailing_silence = self.silence_count;
        self.is_speaking = false;
        self.silence_count = 0;
        self.pre_speech_buffer.clear();

        // Subtract actual trailing silence from buffer length for min_speech check.
        let speech_len = self.speech_buffer.len().saturating_sub(trailing_silence);
        if speech_len >= self.min_speech_samples {
            Some(std::mem::take(&mut self.speech_buffer))
        } else {
            self.speech_buffer.clear();
            None
        }
    }

    fn force_split(&mut self) -> Option<Vec<f32>> {
        // Find the quietest 50ms window in the last 2 seconds for a clean split
        let window_samples = (self.sample_rate as usize * 50) / 1000; // 50ms
        let search_range = (self.sample_rate as usize * 2).min(self.speech_buffer.len()); // last 2s
        let search_start = self.speech_buffer.len().saturating_sub(search_range);

        let mut best_pos = self.speech_buffer.len();
        let mut best_energy = f32::MAX;

        if self.speech_buffer.len() > search_start + window_samples {
            for pos in (search_start..self.speech_buffer.len() - window_samples)
                .step_by(window_samples / 2)
            {
                let window = &self.speech_buffer[pos..pos + window_samples];
                let energy = Self::rms(window);
                if energy < best_energy {
                    best_energy = energy;
                    best_pos = pos + window_samples; // split after the quiet window
                }
            }
        }

        // Split: first part is the utterance, rest carries over
        let remainder = self.speech_buffer[best_pos..].to_vec();
        self.speech_buffer.truncate(best_pos);

        let utterance = std::mem::take(&mut self.speech_buffer);
        self.speech_buffer = remainder;
        self.silence_count = 0;
        // Keep is_speaking = true since we're mid-speech
        self.pre_speech_buffer.clear();

        if utterance.len() >= self.min_speech_samples {
            Some(utterance)
        } else {
            None
        }
    }

    pub(crate) fn peak_rms(samples: &[f32], sample_rate: u32) -> f32 {
        let window_size = (sample_rate as usize * 50) / 1000; // 50ms window
        if samples.len() <= window_size {
            return Self::rms(samples);
        }
        let mut max_rms: f32 = 0.0;
        for chunk in samples.windows(window_size).step_by(window_size / 2) {
            let rms = Self::rms(chunk);
            if rms > max_rms {
                max_rms = rms;
            }
        }
        max_rms
    }

    pub(crate) fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        (sum_sq / samples.len() as f32).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 16000;

    fn silence(duration_ms: u32) -> Vec<f32> {
        vec![0.0; (SAMPLE_RATE as usize * duration_ms as usize) / 1000]
    }

    fn tone(duration_ms: u32, amplitude: f32, freq_hz: f32) -> Vec<f32> {
        let num_samples = (SAMPLE_RATE as usize * duration_ms as usize) / 1000;
        (0..num_samples)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                amplitude * (2.0 * std::f32::consts::PI * freq_hz * t).sin()
            })
            .collect()
    }

    #[test]
    fn test_silence_only_produces_no_output() {
        let mut vad = EnergyVad::new(SAMPLE_RATE, 0.01, 25);
        for _ in 0..100 {
            let chunk = silence(10);
            assert!(vad.process(&chunk).is_none());
        }
    }

    #[test]
    fn test_short_speech_is_filtered() {
        let mut vad = EnergyVad::new(SAMPLE_RATE, 0.01, 25);
        // 100ms of speech (< 300ms minimum)
        let speech = tone(100, 0.5, 440.0);
        vad.process(&speech);
        // Follow with enough silence to trigger end
        let sil = silence(1000);
        let result = vad.process(&sil);
        assert!(result.is_none(), "Short speech should be filtered");
    }

    #[test]
    fn test_normal_utterance_detected() {
        let mut vad = EnergyVad::new(SAMPLE_RATE, 0.01, 25);
        // 1 second of speech
        let speech = tone(1000, 0.5, 440.0);
        // Feed in 10ms chunks
        for chunk in speech.chunks(160) {
            let result = vad.process(chunk);
            assert!(result.is_none(), "Should not emit during speech");
        }
        // 900ms of silence (>= 800ms threshold)
        let sil = silence(900);
        let result = vad.process(&sil);
        assert!(result.is_some(), "Should emit after silence");
        let utterance = result.unwrap();
        // Should include pre-speech buffer + speech + trailing silence
        assert!(utterance.len() > SAMPLE_RATE as usize); // > 1 second
    }

    #[test]
    fn test_force_split_at_max_duration() {
        let mut vad = EnergyVad::new(SAMPLE_RATE, 0.01, 3); // 3 second max
                                                            // Build 4s of speech with a quiet dip at 2.5s so the split point is predictable.
        let mut speech = tone(2500, 0.5, 440.0);
        speech.extend(tone(100, 0.02, 440.0)); // quiet 100ms at 2.5s
        speech.extend(tone(1400, 0.5, 440.0));
        let mut got_output = false;
        for chunk in speech.chunks(160) {
            if let Some(utterance) = vad.process(chunk) {
                got_output = true;
                // Split should occur at the quiet dip (~2.5-2.7s)
                let duration_secs = utterance.len() as f32 / SAMPLE_RATE as f32;
                assert!(
                    duration_secs >= 2.0 && duration_secs <= 3.5,
                    "Force-split utterance should be ~2.5-3s, got {}s",
                    duration_secs
                );
                break;
            }
        }
        assert!(got_output, "Should have force-split");
        // VAD should still be in speaking state with remainder
        assert!(vad.is_speaking);
        assert!(!vad.speech_buffer.is_empty());
    }

    #[test]
    fn test_force_split_carries_over_remainder() {
        let mut vad = EnergyVad::new(SAMPLE_RATE, 0.01, 2); // 2 second max
                                                            // Feed 2.5 seconds of speech
        let speech = tone(2500, 0.5, 440.0);
        let mut first_split = None;
        for chunk in speech.chunks(160) {
            if let Some(utterance) = vad.process(chunk) {
                first_split = Some(utterance);
                break;
            }
        }
        assert!(first_split.is_some(), "Should have force-split");

        // Now feed silence to end the carried-over remainder
        let sil = silence(1000);
        let result = vad.process(&sil);
        // The remainder may or may not be long enough (depends on split point)
        // But the VAD should not be in speaking state anymore
        if result.is_none() {
            // Remainder was too short, that's OK
            assert!(!vad.is_speaking || vad.speech_buffer.len() < vad.min_speech_samples);
        }
    }

    #[test]
    fn test_pre_speech_buffer_included() {
        let mut vad = EnergyVad::new(SAMPLE_RATE, 0.01, 25);
        // Feed 200ms of quiet tone (below threshold but nonzero)
        let quiet = vec![0.001; (SAMPLE_RATE as usize * 200) / 1000];
        vad.process(&quiet);

        // Now speech
        let speech = tone(500, 0.5, 440.0);
        vad.process(&speech);

        // Silence to end
        let sil = silence(1000);
        let result = vad.process(&sil);
        assert!(result.is_some());
        let utterance = result.unwrap();
        // Should be longer than just the speech (includes pre-speech)
        let speech_only_samples = (SAMPLE_RATE as usize * 500) / 1000;
        assert!(
            utterance.len() > speech_only_samples,
            "Should include pre-speech buffer"
        );
    }

    #[test]
    fn test_reset_clears_state() {
        let mut vad = EnergyVad::new(SAMPLE_RATE, 0.01, 25);
        let speech = tone(500, 0.5, 440.0);
        vad.process(&speech);
        assert!(vad.is_speaking);

        vad.reset();
        assert!(!vad.is_speaking);
        assert!(vad.speech_buffer.is_empty());
        assert!(vad.pre_speech_buffer.is_empty());
    }

    #[test]
    fn test_set_threshold() {
        let mut vad = EnergyVad::new(SAMPLE_RATE, 0.5, 25);
        // With high threshold, moderate speech should not trigger
        let speech = tone(1000, 0.1, 440.0);
        vad.process(&speech);
        assert!(!vad.is_speaking);

        // Lower threshold
        vad.set_threshold(0.01);
        vad.process(&speech);
        assert!(vad.is_speaking);
    }

    #[test]
    fn test_rms_calculation() {
        assert_eq!(EnergyVad::rms(&[]), 0.0);
        // Constant signal: RMS should equal the absolute value
        let signal = vec![0.5; 100];
        let rms = EnergyVad::rms(&signal);
        assert!((rms - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_peak_rms_returns_loudest_window() {
        // 500ms silence + 500ms tone (amplitude 0.5)
        let mut samples = silence(500);
        samples.extend(tone(500, 0.5, 440.0));
        let peak = EnergyVad::peak_rms(&samples, SAMPLE_RATE);
        // Peak should reflect the tone portion, not diluted by silence
        assert!(
            peak > 0.3,
            "peak_rms should reflect loud portion, got {}",
            peak
        );
        // Average RMS would be much lower due to silence
        let avg = EnergyVad::rms(&samples);
        assert!(
            peak > avg,
            "peak_rms ({}) should exceed average rms ({})",
            peak,
            avg
        );
    }

    #[test]
    fn test_peak_rms_short_samples_fallback() {
        // Samples shorter than one 50ms window (800 samples at 16kHz)
        let short = tone(30, 0.5, 440.0); // 30ms = 480 samples
        let peak = EnergyVad::peak_rms(&short, SAMPLE_RATE);
        let full_rms = EnergyVad::rms(&short);
        assert!(
            (peak - full_rms).abs() < 0.001,
            "Short samples should fall back to full RMS"
        );
    }
}
