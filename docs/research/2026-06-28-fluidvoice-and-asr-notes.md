# FluidVoice and ASR Notes

Date: 2026-06-28

## FluidVoice Observations

The local clone at `/Users/USER/just-vibes/FluidVoice-db` is a SwiftUI macOS app, not a database dump. Its core mechanics are useful precedent for Slugtale:

- It requires microphone and Accessibility permissions.
- It uses a global event tap for hotkeys.
- It inserts dictated text through a fallback chain: targeted CGEvent unicode insertion, Accessibility insertion, HID events, clipboard paste, menu paste, and finally character-by-character typing.
- It stores settings and transcription history locally using user defaults and files, with optional audio history under Application Support.
- Its ASR providers include FluidAudio-backed Parakeet-family models on Apple Silicon and SwiftWhisper/whisper.cpp for broader Mac support.
- Its README describes audio history, analytics, cloud enhancement, and beta updates as optional; Slugtale's local-only boundary is stricter.

## ASR Findings

Whisper is the lowest-risk cross-platform local baseline. OpenAI Whisper is MIT-licensed, multilingual, and has model sizes from tiny through large/turbo. `whisper.cpp` is the more product-shaped runtime for Slugtale because it is a C/C++ implementation with macOS and Windows support, CPU inference, quantization, Metal/Core ML on Apple Silicon, OpenVINO on Windows/Intel paths, CUDA, Vulkan, and other acceleration routes.

Parakeet is attractive for low latency but riskier as a first cross-platform default. NVIDIA's Parakeet realtime EOU model is English-only, streams with end-of-utterance detection, and is documented around NeMo/Linux/CUDA-class deployment. FluidVoice's fast Parakeet path appears to rely on its Swift/CoreML-oriented FluidAudio integration, which does not directly solve Windows parity.

Decision update: Slugtale v1 is Whisper-first. Parakeet remains a future spike behind a speech recognition boundary rather than the first default.

## Whisper Resource Notes

OpenAI's published Whisper table lists approximate VRAM requirements of about 1 GB for tiny/base, 2 GB for small, 5 GB for medium, 6 GB for turbo, and 10 GB for large. `whisper.cpp` also supports quantized model files that reduce memory and disk usage.

For the current development machine, a MacBook Neo with Apple A18 Pro and 8 GB unified memory, Slugtale should default to a small local model rather than medium/large. `base.en` is the conservative English dictation default; `small.en` is a likely quality upgrade if latency is acceptable. Medium, turbo, and large should be treated as advanced options or later benchmarking targets.

## Product Shape Implication

Slugtale should be treated as a resident desktop agent, not a pure CLI. A CLI can configure, test, and script it, but global hotkeys, microphone permission flow, background lifecycle, tray/menu-bar status, model downloads, and cross-application text insertion all point to a tiny desktop shell.

## Sources

- FluidVoice local clone: `/Users/USER/just-vibes/FluidVoice-db`
- NVIDIA Parakeet realtime model card: https://huggingface.co/nvidia/parakeet_realtime_eou_120m-v1
- OpenAI Whisper README: https://github.com/openai/whisper
- whisper.cpp README: https://github.com/ggml-org/whisper.cpp
- Tauri global shortcut docs: https://v2.tauri.app/plugin/global-shortcut/
- CPAL README: https://github.com/RustAudio/cpal
- Enigo README: https://github.com/enigo-rs/enigo
