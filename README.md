# Audio Agents for Modular Agent

Audio playback, device enumeration, and speech-to-text transcription agents for Modular Agent.

English | [日本語](README_ja.md)

## Features

- **Audio Player** — Play audio data URIs through the default audio output device
- **Audio Device List** — List available audio input devices with unique IDs and names
- **Mic Transcribe** — Capture microphone audio, detect speech via VAD, and transcribe with Whisper

## Installation

Two changes to add this package to [`modular-agent-desktop`](https://github.com/modular-agent/modular-agent-desktop):

1. **`modular-agent-desktop/src-tauri/Cargo.toml`** — add dependency:

   ```toml
   modular-agent-audio = { path = "../../modular-agent-audio" }
   ```

2. **`modular-agent-desktop/src-tauri/src/lib.rs`** — add import:

   ```rust
   #[allow(unused_imports)]
   use modular_agent_audio;
   ```

## Feature Flags

| Feature | Default | Description |
| ------- | ------- | ----------- |
| `capture` | No | Audio device enumeration and microphone capture (enables Audio Device List) |
| `transcribe` | No | Speech-to-text transcription (includes `capture`, enables Mic Transcribe) |

## Audio Player

Plays audio data through system speakers. Accepts data URI strings, decodes and plays them through the default audio output device. Multiple audio inputs are queued and played sequentially.

### Configuration

| Config | Type | Default | Description |
| ------ | ---- | ------- | ----------- |
| volume | number | 1.0 | Playback volume (0.0-1.0) |
| interrupt | boolean | false | Interrupt current playback when new audio arrives |

### Ports

- **Input**: `audio` — Audio data URI string

### Input Format

Data URI format: `data:<mime>;base64,<data>`

Compatible with VoiceVox TTS agent output.

### Supported Formats

- WAV
- MP3
- OGG
- FLAC

Auto-detected by the decoder.

### Playback Behavior

- Audio clips are queued and played sequentially (Player queue)
- `interrupt=true` clears the queue before playing new audio
- Volume is adjustable at runtime via config

## Audio Device List

Lists available audio input devices. Receives any value as a trigger and outputs an array of objects with `id` (unique device identifier) and `name` (human-readable name).

Requires the `capture` feature.

### Ports

- **Input**: `unit` — Any value (trigger)
- **Output**: `devices` — Array of device objects

### Output Format

```json
[
  { "id": "wasapi:{0.0.1.00000000}.{guid}", "name": "Microphone (USB Audio)" },
  { "id": "wasapi:{0.0.1.00000000}.{guid}", "name": "Headset Microphone" }
]
```

The `id` is a platform-specific unique identifier stable across reboots. Use this value for the Mic Transcribe agent's `device` config.

## Mic Transcribe

Source agent (no inputs). Captures microphone audio, segments speech with energy-based VAD, and transcribes using local Whisper (whisper.cpp via whisper-rs).

Requires the `transcribe` feature.

### Configuration

| Config | Type | Default | Description |
| ------ | ---- | ------- | ----------- |
| enabled | boolean | true | Enable/disable mic capture |
| device | string | "" | Audio input device ID (empty = default) |
| language | string | "ja" | Language code for transcription |
| vad_sensitivity | number | 0.01 | VAD sensitivity (RMS threshold, lower = more sensitive) |
| min_volume | number | 0.0 | Minimum peak volume (RMS) to send to Whisper. Utterances below this are discarded. 0 = disabled |
| max_segment_duration | integer | 25 | Max segment duration in seconds (Whisper 30s limit) |

### Global Config

| Config | Type | Description |
| ------ | ---- | ----------- |
| model_path | string | Path to Whisper GGML model file (e.g. ggml-medium.bin) |

Download models from <https://huggingface.co/ggerganov/whisper.cpp/tree/main>

### Ports

- **Output**: `text` — Transcribed text for each detected utterance
- **Output**: `status` — State changes: `"recording_started"`, `"recording_stopped"`, `"error: ..."`

### Build Requirements

- C/C++ compiler + CMake (for whisper.cpp, built from source by whisper-rs)
- **Windows MSVC**: Add `.cargo/config.toml` to set `CMAKE_MSVC_RUNTIME_LIBRARY` to fix CRT mismatch (`/MD` vs `/MT`) between whisper-rs-sys and other crates:

  ```toml
  [env]
  CMAKE_POLICY_DEFAULT_CMP0091 = "NEW"
  CMAKE_MSVC_RUNTIME_LIBRARY = "MultiThreaded"
  ```

- **macOS**: `NSMicrophoneUsageDescription` in Info.plist for mic permission
- **Linux**: `alsa-lib` dev headers required

## Architecture

- **Audio Player**: Dedicated OS thread with `mpsc` channel for playback isolation from the async runtime. Communicates via `AudioCommand` messages (Play, SetVolume, Clear, Shutdown).
- **Mic Transcribe**: OS thread + `rtrb` lock-free ring buffer for real-time audio callback safety. cpal callback → rtrb → processing thread → mono conversion → resample (16kHz) → VAD → Whisper. Runtime config changes (`vad_sensitivity`/`min_volume`/`language`) via `Arc<Mutex>`.

## Key Dependencies

- [rodio](https://crates.io/crates/rodio) — Audio playback and decoding
- [base64](https://crates.io/crates/base64) — Data URI decoding
- [cpal](https://crates.io/crates/cpal) — Audio device enumeration and input capture (optional, `capture` feature)
- [rtrb](https://crates.io/crates/rtrb) — Lock-free ring buffer for real-time audio (optional, `capture` feature)
- [whisper-rs](https://crates.io/crates/whisper-rs) — Whisper.cpp bindings for speech-to-text (optional, `transcribe` feature)
- [rubato](https://crates.io/crates/rubato) — Audio resampling to 16kHz (optional, `transcribe` feature)

## License

Apache-2.0 OR MIT
