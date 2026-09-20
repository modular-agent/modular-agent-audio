# Audio Modules for Modular Agent

Audio playback, device enumeration, and speech-to-text transcription modules for Modular Agent.

English | [日本語](README_ja.md)

## Features

- **Audio Player** — Play audio data URIs through the default audio output device
- **Audio Device List** — List available audio capture devices (plus loopback sources on Windows) with unique IDs and names
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
| `transcribe` | No | Whisper speech-to-text engine (includes `capture`, enables Mic Transcribe) |
| `sherpa` | No | sherpa-onnx engine (ReazonSpeech) and Silero VAD (includes `capture`, enables Mic Transcribe) |

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

Compatible with VoiceVox TTS module output.

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

Lists available audio capture devices. Receives any value as a trigger and outputs an array of objects with `id` (unique device identifier), `name` (human-readable name), and `kind` (`"input"` or `"loopback"`). On Windows, output devices are appended as `"loopback"` entries; see [Loopback capture (Windows)](#loopback-capture-windows).

Requires the `capture` feature.

### Ports

- **Input**: `unit` — Any value (trigger)
- **Output**: `devices` — Array of device objects

### Output Format

```json
[
  { "id": "wasapi:{0.0.1.00000000}.{guid}", "name": "Microphone (USB Audio)", "kind": "input" },
  { "id": "wasapi:{0.0.1.00000000}.{guid}", "name": "Headset Microphone", "kind": "input" },
  { "id": "wasapi:{0.0.0.00000000}.{guid}", "name": "Speakers (Realtek Audio)", "kind": "loopback" }
]
```

The `id` is a platform-specific unique identifier stable across reboots. Use this value for the Mic Transcribe module's `device` config.

## Mic Transcribe

Source module (no inputs). Captures microphone audio, segments speech with a VAD, and transcribes it locally with Whisper or sherpa-onnx.

Requires the `transcribe` feature, the `sherpa` feature, or both.

### Engines

| Engine | Feature | Notes |
| ------ | ------- | ----- |
| `whisper` | `transcribe` | whisper.cpp via whisper-rs. Any Whisper GGML model; GPU via the `transcribe-*` features |
| `sherpa` | `sherpa` | sherpa-onnx offline transducer (ReazonSpeech ja-en). CPU only, about 50x faster than real time with the int8 model |

Set `engine` to pick one; with both features built in, empty means `whisper`.

Silero VAD replaces the energy-based VAD when `silero_vad_path` is set (requires the `sherpa` feature). It works with either engine and ends utterances after 0.35 s of silence.

#### Model setup for `sherpa`

Download and extract from <https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/>:

- `sherpa-onnx-zipformer-ja-en-reazonspeech-2025-01-17.tar.bz2` → set `sherpa_model_dir` to the extracted directory (int8 files are picked automatically)
- `silero_vad.onnx` → set `silero_vad_path` to the file

Recommended settings for a conversational agent: `engine = "sherpa"`, `silero_vad_path` set, `partial_interval = 0.5`, `max_segment_duration = 12`.

### Configuration

| Config | Type | Default | Description |
| ------ | ---- | ------- | ----------- |
| enabled | boolean | true | Enable/disable mic capture |
| device | string | "" | Audio input device ID (empty = default mic, `"loopback"` = default output on Windows) |
| engine | string | "" | Transcription engine: `"whisper"` or `"sherpa"` (empty = whisper when built in, otherwise sherpa) |
| language | string | "ja" | Language code for transcription (Whisper only) |
| vad_sensitivity | number | 0.01 | Energy VAD sensitivity (RMS threshold, lower = more sensitive) |
| silero_threshold | number | 0.5 | Silero VAD speech probability threshold (used when `silero_vad_path` is set) |
| min_volume | number | 0.0 | Minimum peak volume (RMS) to send to Whisper. Utterances below this are discarded. 0 = disabled |
| max_segment_duration | integer | 25 | Max segment duration in seconds before a force-split (Whisper's limit is 30; 12 suits conversational use) |
| silence_duration_ms | integer | 800 | Energy VAD: trailing silence in milliseconds that ends an utterance. Lower values finalize sooner but split mid-sentence more often; 400–500 suits conversational use |
| partial_interval | number | 0.0 | Seconds of speech between partial results while an utterance is in progress. 0 = disabled. Each partial re-decodes the last 8 s: cheap with `sherpa` (0.5), Whisper only on GPU builds (0.5–1.0) |

### Global Config

| Config | Type | Description |
| ------ | ---- | ----------- |
| model_path | string | Whisper: path to a GGML model file (e.g. ggml-medium.bin) |
| sherpa_model_dir | string | sherpa: directory holding the transducer encoder/decoder/joiner `.onnx` files and `tokens.txt` |
| silero_vad_path | string | Path to `silero_vad.onnx`. Empty = energy-based VAD |

Whisper models: <https://huggingface.co/ggerganov/whisper.cpp/tree/main>. Models are never downloaded automatically.

### Ports

- **Output**: `text` — Transcribed text for each detected utterance
- **Output**: `partial` — Interim text for the utterance in progress, emitted every `partial_interval` seconds of speech
- **Output**: `status` — State changes: `"recording_started"`, `"recording_stopped"`, `"error: ..."`,
  `"dropped: transcription backlog"` (an utterance was discarded because inference could not keep up),
  `"overrun: input samples dropped"` (the capture ring buffer overflowed)

### Loopback capture (Windows)

On Windows, Mic Transcribe can transcribe whatever is being played through an output device instead of a microphone (WASAPI loopback). Set `device` to `"loopback"` to follow the default output device, or to the `id` of a `"loopback"` entry from Audio Device List to pick a specific one.

Loopback reads a copy of the shared-mode mix, so the audio keeps playing through the speakers as usual. Nothing is delivered while no audio is playing, so `text` stays silent until something is played. Audio from applications using WASAPI exclusive mode is not captured. Other platforms reject `"loopback"` with a config error.

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
- **`sherpa` feature**: no compiler needed for sherpa-onnx itself. `sherpa-onnx-sys` downloads a prebuilt static library from GitHub Releases on first build (about 123 MB on Windows x64) and caches it under `target/`. For offline builds set `SHERPA_ONNX_ARCHIVE_DIR` to a directory holding the release tarball, or `SHERPA_ONNX_LIB_DIR` to an extracted `lib` directory

## Architecture

- **Audio Player**: Dedicated OS thread with `mpsc` channel for playback isolation from the async runtime. Communicates via `AudioCommand` messages (Play, SetVolume, Clear, Shutdown).
- **Mic Transcribe**: OS thread + `rtrb` lock-free ring buffer for real-time audio callback safety. cpal callback → rtrb → processing thread → mono conversion → resample (16kHz) → VAD (energy or Silero) → job queue → inference thread → engine (Whisper or sherpa-onnx). Inference runs on a separate thread so the audio path never stalls; when it falls behind, whole utterances are dropped and reported on `status` rather than losing input samples. Runtime config changes (`vad_sensitivity`/`min_volume`/`language`) via `Arc<Mutex>`.

## Key Dependencies

- [rodio](https://crates.io/crates/rodio) — Audio playback and decoding
- [base64](https://crates.io/crates/base64) — Data URI decoding
- [cpal](https://crates.io/crates/cpal) — Audio device enumeration and input capture (optional, `capture` feature)
- [rtrb](https://crates.io/crates/rtrb) — Lock-free ring buffer for real-time audio (optional, `capture` feature)
- [whisper-rs](https://crates.io/crates/whisper-rs) — Whisper.cpp bindings for speech-to-text (optional, `transcribe` feature)
- [rubato](https://crates.io/crates/rubato) — Audio resampling to 16kHz (optional, `transcribe` feature)

## License

Apache-2.0 OR MIT
