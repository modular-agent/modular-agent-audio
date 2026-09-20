# Audio Modules for Modular Agent

Modular Agent 用のオーディオ再生・デバイス列挙・音声文字起こしモジュール。

[English](README.md) | 日本語

## 機能

- **Audio Player** — デフォルトのオーディオ出力デバイスを通じてオーディオデータ URI を再生
- **Audio Device List** — 利用可能なオーディオキャプチャデバイス (Windows ではループバックソースも) の一覧をユニーク ID と名前付きで取得
- **Mic Transcribe** — マイク音声をキャプチャし、VAD で発話を検出、Whisper で文字起こし

## インストール

[`modular-agent-desktop`](https://github.com/modular-agent/modular-agent-desktop) にこのパッケージを追加するには、2つの変更が必要です:

1. **`modular-agent-desktop/src-tauri/Cargo.toml`** — 依存関係を追加:

   ```toml
   modular-agent-audio = { path = "../../modular-agent-audio" }
   ```

2. **`modular-agent-desktop/src-tauri/src/lib.rs`** — インポートを追加:

   ```rust
   #[allow(unused_imports)]
   use modular_agent_audio;
   ```

## フィーチャーフラグ

| フィーチャー | デフォルト | 説明 |
| ------------ | ---------- | ---- |
| `capture` | No | オーディオデバイス列挙とマイクキャプチャ (Audio Device List を有効化) |
| `transcribe` | No | 音声文字起こし (`capture` を含む、Mic Transcribe を有効化) |

## Audio Player

システムスピーカーを通じて音声を再生します。データ URI 文字列を受け取り、デコードしてデフォルトのオーディオ出力デバイスから再生します。複数の音声入力はキューに追加され、順番に再生されます。

### 設定

| 設定項目 | 型 | デフォルト値 | 説明 |
| -------- | -- | ------------ | ---- |
| volume | number | 1.0 | 再生音量 (0.0-1.0) |
| interrupt | boolean | false | 新しい音声が到着した際に再生中の音声を中断する |

### ポート

- **入力**: `audio` — オーディオデータ URI 文字列

### 入力フォーマット

データ URI 形式: `data:<mime>;base64,<data>`

VoiceVox TTS モジュールの出力と互換性があります。

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

## Audio Device List

利用可能なオーディオキャプチャデバイスを一覧表示します。任意の値をトリガーとして受け取り、`id` (ユニークなデバイス識別子)、`name` (人間が読める名前)、`kind` (`"input"` または `"loopback"`) を持つオブジェクトの配列を出力します。Windows では出力デバイスが `"loopback"` エントリとして末尾に追加されます。[ループバックキャプチャ (Windows)](#ループバックキャプチャ-windows) を参照してください。

`capture` フィーチャーが必要です。

### ポート

- **入力**: `unit` — 任意の値 (トリガー)
- **出力**: `devices` — デバイスオブジェクトの配列

### 出力フォーマット

```json
[
  { "id": "wasapi:{0.0.1.00000000}.{guid}", "name": "Microphone (USB Audio)", "kind": "input" },
  { "id": "wasapi:{0.0.1.00000000}.{guid}", "name": "Headset Microphone", "kind": "input" },
  { "id": "wasapi:{0.0.0.00000000}.{guid}", "name": "スピーカー (Realtek Audio)", "kind": "loopback" }
]
```

`id` はプラットフォーム固有のユニーク識別子で、再起動後も安定しています。この値を Mic Transcribe モジュールの `device` 設定に使用します。

## Mic Transcribe

ソースモジュール (入力なし)。マイク音声をキャプチャし、エネルギーベースの VAD で発話を区間検出し、ローカル Whisper (whisper.cpp via whisper-rs) で文字起こしします。

`transcribe` フィーチャーが必要です。

### 設定

| 設定項目 | 型 | デフォルト値 | 説明 |
| -------- | -- | ------------ | ---- |
| enabled | boolean | true | マイクキャプチャの有効/無効 |
| device | string | "" | オーディオ入力デバイス ID (空 = デフォルトマイク、`"loopback"` = Windows のデフォルト出力) |
| language | string | "ja" | 文字起こしの言語コード |
| vad_sensitivity | number | 0.01 | VAD 感度 (RMS 閾値、低いほど感度が高い) |
| min_volume | number | 0.0 | Whisper に送る最低ピーク音量 (RMS)。これ未満の発話は破棄される。0 = 無効 |
| max_segment_duration | integer | 25 | 最大セグメント長 (秒、Whisper 30秒制限) |

### グローバル設定

| 設定項目 | 型 | 説明 |
| -------- | -- | ---- |
| model_path | string | Whisper GGML モデルファイルのパス (例: ggml-medium.bin) |

モデルは <https://huggingface.co/ggerganov/whisper.cpp/tree/main> からダウンロードできます。

### ポート

- **出力**: `text` — 検出された発話ごとの文字起こしテキスト
- **出力**: `status` — 状態変化: `"recording_started"`, `"recording_stopped"`, `"error: ..."`

### ループバックキャプチャ (Windows)

Windows では、マイクの代わりに出力デバイスで再生中の音を文字起こしできます (WASAPI ループバック)。`device` に `"loopback"` を設定するとデフォルト出力デバイスに追従し、Audio Device List の `"loopback"` エントリの `id` を設定すると特定の出力デバイスを選べます。

ループバックは共有モードのミックスの複製を読むだけなので、スピーカーからは通常どおり音が出続けます。何も再生されていない間はデータが届かないため、`text` は音が再生されるまで出力されません。WASAPI 排他モードで再生しているアプリの音はキャプチャできません。Windows 以外のプラットフォームでは `"loopback"` は設定エラーになります。

### ビルド要件

- C/C++ コンパイラ + CMake (whisper.cpp 用、whisper-rs によりソースからビルド)
- **Windows MSVC**: `.cargo/config.toml` で `CMAKE_MSVC_RUNTIME_LIBRARY` を設定し、whisper-rs-sys と他クレート間の CRT ミスマッチ (`/MD` vs `/MT`) を修正:

  ```toml
  [env]
  CMAKE_POLICY_DEFAULT_CMP0091 = "NEW"
  CMAKE_MSVC_RUNTIME_LIBRARY = "MultiThreaded"
  ```

- **macOS**: マイク権限のため Info.plist に `NSMicrophoneUsageDescription` が必要
- **Linux**: `alsa-lib` 開発ヘッダーが必要

## アーキテクチャ

- **Audio Player**: 非同期ランタイムからの再生分離のため、`mpsc` チャネルを持つ専用 OS スレッドを使用。`AudioCommand` メッセージ (Play, SetVolume, Clear, Shutdown) で通信。
- **Mic Transcribe**: OS スレッド + `rtrb` ロックフリーリングバッファによるリアルタイムオーディオコールバック安全性。cpal コールバック → rtrb → 処理スレッド → モノラル変換 → リサンプル (16kHz) → VAD → Whisper。ランタイム設定変更 (`vad_sensitivity`/`min_volume`/`language`) は `Arc<Mutex>` 経由。

## 主要な依存クレート

- [rodio](https://crates.io/crates/rodio) — オーディオ再生とデコード
- [base64](https://crates.io/crates/base64) — データ URI デコード
- [cpal](https://crates.io/crates/cpal) — オーディオデバイス列挙と入力キャプチャ (オプション、`capture` フィーチャー)
- [rtrb](https://crates.io/crates/rtrb) — リアルタイムオーディオ用ロックフリーリングバッファ (オプション、`capture` フィーチャー)
- [whisper-rs](https://crates.io/crates/whisper-rs) — Whisper.cpp バインディング (オプション、`transcribe` フィーチャー)
- [rubato](https://crates.io/crates/rubato) — 16kHz へのオーディオリサンプリング (オプション、`transcribe` フィーチャー)

## ライセンス

Apache-2.0 OR MIT
