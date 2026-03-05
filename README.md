# Audio Agents for Modular Agent

Audio playback agents for Modular Agent. Plays audio data through system speakers.

English | [日本語](README_ja.md)

## Features

- **Audio Player** — Play audio data URIs through the default audio output device

## Installation

Two changes to add this package to [`modular-agent-desktop`](https://github.com/modular-agent/modular-agent-desktop):

1. **`modular-agent-desktop/src-tauri/Cargo.toml`** — add dependency:

   ```toml
   modular-agent-audio = { git = "https://github.com/modular-agent/modular-agent-audio.git", tag = "v0.1.0" }
   ```

2. **`modular-agent-desktop/src-tauri/src/lib.rs`** — add import:

   ```rust
   #[allow(unused_imports)]
   use modular_agent_audio;
   ```

## Audio Player

### Configuration

| Config | Type | Default | Description |
| ------ | ---- | ------- | ----------- |
| interrupt | boolean | false | Interrupt current playback when new audio arrives |
| volume | number | 1.0 | Playback volume (0.0-1.0) |

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

- Audio clips are queued and played sequentially (Sink queue)
- `interrupt=true` clears the queue before playing new audio
- Volume is adjustable at runtime via config

## Architecture

Uses a dedicated OS thread with an `mpsc` channel for audio commands. This is necessary because `rodio::OutputStream` is `!Send + !Sync`. The agent communicates with the audio thread via `AudioCommand` messages (Play, SetVolume, Clear, Shutdown).

## Key Dependencies

- [rodio](https://crates.io/crates/rodio) — Audio playback and decoding
- [base64](https://crates.io/crates/base64) — Data URI decoding

## License

Apache-2.0 OR MIT
