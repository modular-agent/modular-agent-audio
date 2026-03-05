# Audio Agents for Modular Agent

Modular Agent 用のオーディオ再生エージェント。システムスピーカーを通じて音声データを再生します。

[English](README.md) | 日本語

## 機能

- **Audio Player** — デフォルトのオーディオ出力デバイスを通じてオーディオデータ URI を再生

## インストール

[`modular-agent-desktop`](https://github.com/modular-agent/modular-agent-desktop) にこのパッケージを追加するには、2つの変更が必要です:

1. **`modular-agent-desktop/src-tauri/Cargo.toml`** — 依存関係を追加:

   ```toml
   modular-agent-audio = { git = "https://github.com/modular-agent/modular-agent-audio.git", tag = "v0.1.0" }
   ```

2. **`modular-agent-desktop/src-tauri/src/lib.rs`** — インポートを追加:

   ```rust
   #[allow(unused_imports)]
   use modular_agent_audio;
   ```

## Audio Player

### 設定

| 設定項目 | 型 | デフォルト値 | 説明 |
| -------- | -- | ------------ | ---- |
| interrupt | boolean | false | 新しい音声が到着した際に再生中の音声を中断する |
| volume | number | 1.0 | 再生音量 (0.0-1.0) |

### ポート

- **入力**: `audio` — オーディオデータ URI 文字列

### 入力フォーマット

データ URI 形式: `data:<mime>;base64,<data>`

VoiceVox TTS エージェントの出力と互換性があります。

### 対応フォーマット

- WAV
- MP3
- OGG
- FLAC

デコーダーにより自動検出されます。

### 再生動作

- オーディオクリップはキューに追加され、順番に再生されます (Player キュー)
- `interrupt=true` にすると、新しい音声の再生前にキューがクリアされます
- 音量は設定から実行時に変更可能です

## アーキテクチャ

非同期ランタイムからのオーディオ再生の分離のため、`mpsc` チャネルを持つ専用 OS スレッドを使用します。エージェントは `AudioCommand` メッセージ (Play, SetVolume, Clear, Shutdown) を通じてオーディオスレッドと通信します。

## ライセンス

Apache-2.0 OR MIT
