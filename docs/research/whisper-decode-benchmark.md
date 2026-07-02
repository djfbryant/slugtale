# Whisper decode benchmark — Transcription Speed Profile values

Date: 2026-07-02 · Issue: slugtale-1lq

## Why

The Transcription Speed Profile originally mapped Fast/Balanced/Accurate to
`Greedy { best_of: 1/5/10 }`. whisper.cpp ignores greedy `best_of` at its
default temperature (it only affects temperature-fallback re-decoding), so
Balanced and Accurate were effectively identical to Fast, and the pre-profile
quality level (`BeamSearch { beam_size: 5 }`) was unreachable. The issue also
required decode strategy and thread count to be chosen from measured numbers.

## Method

`src-tauri/examples/decode_benchmark.rs` decodes 16 kHz mono f32 WAV clips with
each candidate strategy and prints per-clip latency and transcript. Each timing
is the second of two runs (the first warms model upload/caches).

```sh
cd src-tauri
cargo run --release --example decode_benchmark \
  --features local-whisper-runtime,local-whisper-runtime-metal -- \
  <ggml-base.en.bin> <clip1.wav> [clip2.wav ...]
```

Clips were real speech synthesized with macOS `say` (Samantha and Daniel
voices) at three lengths — 2.3 s command, 7.6 s sentence, 15.8 s paragraph —
converted with `afconvert -f WAVE -d LEF32@16000 -c 1`. Limitation: synthetic
speech is cleaner than live dictation audio; re-check with recorded dictation
clips if accuracy complaints appear.

Hardware: Apple silicon Mac, 6 logical cores, Metal enabled
(`local-whisper-runtime-metal`, the configuration macOS builds ship).
Model: ggml `base.en`.

## Results

Latency in ms (two independent runs where taken; ordering was stable):

| Strategy | 2.3 s clip | 7.6 s clip | 15.8 s clip |
|---|---|---|---|
| Greedy best_of=1, all threads | 288 | 301 / 376 | 446 / 424 |
| Greedy best_of=1, 4 threads (whisper default) | 224 | 334 / 334 | 361 / 436 |
| BeamSearch beam_size=2, all threads | 241 | 325 / 307 | 384 / 405 |
| BeamSearch beam_size=5 (previous default), all threads | 282 | 424 / 308 | 559 / 716 |

Accuracy: every strategy transcribed every clip essentially perfectly (one
article/punctuation-level difference between strategies on the long clip), so
accuracy stayed within tolerance across the board on these clips.

## Decisions

- **Fast → `Greedy { best_of: 1 }`** — lowest decoder overhead; fastest or
  tied on every clip.
- **Balanced → `BeamSearch { beam_size: 2 }`** — within noise of greedy
  (≤ ~10% slower) while keeping a real accuracy hedge; matches the issue's
  "small beam (2)" candidate.
- **Accurate → `BeamSearch { beam_size: 5 }`** — restores the pre-profile
  default quality; measurably slower (25–45%+) on longer clips, which is the
  accepted trade.
- **`n_threads` stays `available_parallelism()`** — with Metal offloading the
  encoder, 4 vs 6 threads differed only within run-to-run noise, so thread
  count is not the bottleneck; deriving from the host follows the issue and
  costs nothing.
- **Cargo features: `local-whisper-runtime-metal` on macOS.** whisper-rs's
  `coreml` feature was evaluated and deferred: it needs a separately converted
  `.mlmodelc` encoder shipped next to the ggml model and only accelerates the
  encoder, which Metal already offloads.
