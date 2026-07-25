# Selective local ASR validation — method

Date: 2026-07-25 · Issue: slugtale-vjs.5

## Status

**No measurements have been taken yet.** This document describes the corpus,
the harness, and the offline/leakage checks slugtale-vjs.5 asks for. Every
table below is empty on purpose. Filling one in means a maintainer has
actually run the harness in `src-tauri/examples/asr_eval.rs` against a real
corpus recorded on their own machine — nothing here should be treated as a
result until that has happened, and this document should be updated in place
(or superseded by a dated results doc) once it has.

The harness this method describes measures aggregate statistics only. It
never prints a transcript, a reference, or a confidence value — see the doc
comment at the top of `asr_eval.rs` for why that is load-bearing rather than
incidental. That is also why this document is safe to keep open in the same
window as a beads issue: nothing you paste out of the harness's own output
can leak what the maintainer said while recording the corpus.

## 1. Recording the corpus

Slugtale-9dv's design doc
(`docs/research/2026-07-24-small-local-asr-and-model-collaboration.md`, Phase
A) asks for at least 100 labelled clips covering:

- 1–3 second commands and 5–30 second dictation;
- quiet speech, fan/keyboard noise, and distance from the microphone;
- natural hesitations and self-corrections;
- numbers, punctuation, contractions, and capitalization;
- names and Slugtale/software terms (jargon a generic benchmark would not
  cover);
- a silence/non-speech set, to measure hallucination rate.

Record each clip as its own file, then convert to the format the harness (and
`decode_benchmark.rs`) require — 16 kHz mono 32-bit float WAV:

```sh
afconvert -f WAVE -d LEF32@16000 -c 1 <input> <clip-id>.wav
```

Next to every `<clip-id>.wav`, write `<clip-id>.txt` containing the exact
reference transcript for that clip — what the maintainer actually said,
normalized only in the sense of being the correct sentence, not lowercased or
depunctuated (the harness does that normalization itself for WER, and needs
the real punctuation/capitalization to score those separately). For a
silence/non-speech clip, `<clip-id>.txt` should exist and be empty; the
harness treats an empty reference as "this clip is not supposed to produce
text" and scores any output against it as a hallucination rather than a word
error.

A suggested naming convention that keeps categories visible without needing a
separate manifest:

```
corpus/
  001-command-clean.wav       001-command-clean.txt
  002-dictation-noisy.wav     002-dictation-noisy.txt
  003-names-jargon.wav        003-names-jargon.txt
  004-silence.wav             004-silence.txt   (empty file)
  ...
  terms.txt                   (optional — see below)
```

`terms.txt` is optional: one proper name or piece of jargon per line (e.g.
`Slugtale`, `Parakeet`, `whisper.cpp`). The harness computes name/term recall
by checking, for every clip whose reference actually contains a term, whether
the hypothesis also contains it.

This corpus is disposable evaluation material, not production dictation
history — do not fold it into the app's own model directories or settings,
and do not commit raw clips or transcripts to the repository (they are the
maintainer's own recorded speech).

## 2. Running the harness

```sh
cd src-tauri
cargo run --release --example asr_eval --features local-whisper-runtime -- \
  --corpus <path/to/corpus> \
  --whisper-model <ggml-base.en.bin>
```

Add engines as their assets are installed:

```sh
# Parakeet standalone (needs the assets installed via Settings, or
# install_parakeet_assets in a one-off script):
cargo run --release --example asr_eval --features local-whisper-runtime -- \
  --corpus <path/to/corpus> --parakeet-assets <path/to/parakeet/assets>

# Apple SpeechTranscriber standalone, macOS only:
cargo run --release --example asr_eval \
  --features local-whisper-runtime,apple-speech-runtime -- \
  --corpus <path/to/corpus> --apple-speech
```

**Run one engine per invocation when memory matters.** Peak resident memory
comes from the OS's `ru_maxrss`, which is a whole-process high-water mark that
only ever grows — see the comment on `peak_resident_memory_bytes` in
`asr_eval.rs`. A combined run's number reflects whichever engines had already
loaded by the time it printed, not one engine in isolation. Once the
per-engine numbers are recorded, run the router:

```sh
cargo run --release --example asr_eval \
  --features local-whisper-runtime,local-parakeet-runtime -- \
  --corpus <path/to/corpus> \
  --whisper-model <ggml-base.en.bin> --parakeet-assets <path/to/parakeet/assets> \
  --routed
```

Add `--terms terms.txt` to any invocation for the name-recall stat, and
`--json` to get a machine-readable report instead of the printed table (both
forms carry the same fields; see `HarnessReport` in `asr_eval.rs`).

Every run prints, per engine and for the routed path: mean normalized WER,
empty-output rate, silence hallucination rate, proper-term recall, punctuation
accuracy, capitalization accuracy, cold and warm p50/p95 latency, and (for the
routed path) escalation rate broken down by reason, and selector win/loss/tie
rate among escalated clips. It also prints one peak-resident-memory reading
for the whole process at the end. None of it is a transcript.

## 3. The network-denied test

This checks the product's non-negotiable data boundary
(`docs/research/2026-07-24-small-local-asr-and-model-collaboration.md`):
after model assets are installed, transcription must keep working with the
network denied, and nothing should attempt a remote fallback. Install the
assets first (over the network, as an explicit user action), then deny
networking and re-run the harness.

### macOS

Two options, from least to most invasive:

**`sandbox-exec`** (fastest — wraps just this one process):

```sh
sandbox-exec -p '(version 1)(deny network*)(allow default)' \
  ./target/release/examples/asr_eval \
  --corpus <path/to/corpus> --parakeet-assets <path/to/parakeet/assets>
```

`sandbox-exec` is an Apple-private, technically deprecated interface, but it
remains present through the OS versions Slugtale targets and is the quickest
way to deny one process's sockets without touching the rest of the machine.
If it stops working on a future macOS, fall back to the interface method
below.

**Disable the network interface** (more invasive, but unambiguous):

```sh
networksetup -setairportpower en0 off   # confirm en0 is the Wi-Fi device with: networksetup -listallhardwareports
# ... run the harness ...
networksetup -setairportpower en0 on
```

Either way, confirm the run completes with the same (or a very close) mean
WER as a networked run, and that no engine reports
`EngineUnavailable::ProbeFailed` or a runtime error that only shows up with
networking off — a networking-shaped failure here would mean something is
reaching for the network on the inference path, which is exactly what this
test exists to catch.

### Linux

`unshare` puts the process in a network namespace with no interfaces at all
(not even loopback, unless you configure one):

```sh
sudo unshare --net --map-root-user -- \
  ./target/release/examples/asr_eval \
  --corpus <path/to/corpus> --parakeet-assets <path/to/parakeet/assets>
```

If `unshare` is unavailable, `firejail --net=none -- <command>` is an
equivalent sandboxed alternative on distributions that ship it.

## 4. Inspecting logs and traffic for leakage

Two independent checks, because either one missing a leak is a real risk:

**Traffic.** While a real dictation runs (not the harness — the actual app,
since that is what ships), capture packets and confirm nothing leaves the
host for the ASR path:

```sh
# macOS
sudo tcpdump -i any -n 'not port 53' &
# ... dictate, stop capture ...

# Linux
sudo tcpdump -i any -n 'not port 53' &
```

(DNS is excluded above only because unrelated OS chatter is noisy; if the
model-install path is being exercised, expect and verify DNS/HTTPS traffic to
the pinned model host and nothing else.) Zero packets is the expected result
for a dictation once assets are already installed.

**Logs.** Read whatever Slugtale actually writes to its Local Diagnostic Log
during a dictation and confirm it contains only the closed, non-content types
this codebase defines for that purpose — `RoutingDiagnostics`,
`EngineUnavailable`, latency numbers — never a transcript, alternative, or
confidence value:

```sh
# macOS unified log, filtered to the app while dictating:
log stream --predicate 'process == "slugtale"' --style compact
```

As a mechanical sanity check, grep whatever the diagnostic log or this
harness prints for any word that appears in the corpus's reference
transcripts. It should never match — `second_opinion.rs` already has a unit
test asserting this for `RoutingDiagnostics`
(`routing_diagnostics_carry_reason_codes_and_no_speech`); this step is the
same assertion applied to the real log output and to a harness run's stdout,
by hand, once.

## 5. Results — empty, pending a real corpus run

Fill in once `asr_eval.rs` has run against a maintainer-recorded corpus of at
least 100 clips. Every cell below is a placeholder.

Hardware: _(fill in — chip, RAM, OS version)_
Corpus: _(fill in — clip count, total duration, recording date)_

### Per-engine (standalone, memory-isolated runs)

| Engine | Clips scored | Mean WER | Empty-output rate | Silence hallucination rate | Term recall | Punctuation accuracy | Capitalization accuracy | Cold latency | Warm p50 | Warm p95 | Peak RSS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Whisper `base.en` | | | | | | | | | | | |
| Parakeet TDT v2 | | | | | | | | | | | |
| Apple SpeechTranscriber | | | | | | | | | | | |

### Routed (Second Opinion)

| Second engine | Escalation rate | empty-transcript | low-confidence | repeated-phrase | implausibly-short | Selector win | Selector loss | Selector tie | Routed mean WER | Cold latency | Warm p50 | Warm p95 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Parakeet TDT v2 | | | | | | | | | | | | |
| Apple SpeechTranscriber | | | | | | | | | | | |

### Network-denied test

| Engine | Platform | Result | Notes |
|---|---|---|---|
| Parakeet TDT v2 | macOS | | |
| Parakeet TDT v2 | Linux | | |
| Apple SpeechTranscriber | macOS | | |

### Log/traffic leakage check

| Check | Result | Notes |
|---|---|---|
| Packet capture during a real dictation shows zero outbound traffic | | |
| Local Diagnostic Log during a real dictation contains only closed diagnostic types | | |
| No reference-corpus word appears in diagnostic log or harness output | | |

### Sustained memory and thermal behaviour

Out of scope for `asr_eval.rs` itself — it reports one peak-RSS number per
process, not a time series, and has no thermal reading. For a longer batch
run (the whole corpus, back to back, on one warm engine), sample the OS
directly alongside it:

```sh
# macOS: thermal pressure and package power over the run
sudo powermetrics --samplers thermal,cpu_power -i 1000 > powermetrics.log &

# Linux: per-core frequency/throttling and package power
sudo turbostat --interval 1 > turbostat.log &
```

| Engine | Sustained RSS (steady-state, not peak) | Thermal pressure observed | Notes |
|---|---|---|---|
| Whisper `base.en` | | | |
| Parakeet TDT v2 | | | |
| Apple SpeechTranscriber | | | |

## 6. What decides Parakeet vs. Apple as primary

Not decided here. Once the tables above are filled in, the comparison this
issue's acceptance criteria ask for — which engine (if either) should replace
Whisper as primary, and what escalation thresholds
(`EscalationPolicy::default()` in `second_opinion.rs`) the measured data
actually supports — belongs in a dated results doc that cites this method
doc and the real numbers, not in a rewrite of this file.
