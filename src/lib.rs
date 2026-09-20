#![recursion_limit = "256"]

pub mod player;

#[cfg(feature = "capture")]
pub mod vad;

#[cfg(feature = "capture")]
pub mod device_list;

#[cfg(any(feature = "transcribe", feature = "sherpa"))]
pub mod mic_transcribe;

#[cfg(any(feature = "transcribe", feature = "sherpa"))]
mod transcriber;

#[cfg(feature = "transcribe")]
mod engine_whisper;

#[cfg(feature = "sherpa")]
mod engine_sherpa;

#[cfg(feature = "sherpa")]
mod silero_vad;
