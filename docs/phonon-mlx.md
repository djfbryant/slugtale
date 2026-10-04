# Phonon-2 on Apple silicon

Normal `npm run dev`, `npm run build`, and `npm run macos:install` feature
resolution includes `local-phonon-mlx` on macOS. Apple silicon with macOS 14+
uses Fermion MLX. Intel Macs, older macOS, Windows, Linux, and builds without
that feature retain the existing ONNX provider. That provider still needs
`local-parakeet-runtime`. Parakeet itself remains on its existing runtime.
Explicit `--features` arguments to the build launcher override its defaults.

MLX runs the model on the Mac GPU. ONNX is the portable model format and
runtime used by the previous Phonon provider. Both run locally. They use
different number formats, so their words need not be identical.

## Setup and distribution

Settings → Transcription Engines → Phonon-2 → Install downloads a private
runtime under the models directory's `phonon-2-mlx/`. No system Python,
Homebrew, user shell environment, or pre-existing Fermion install is required.
The existing `phonon-2/` ONNX directory is separate and is not migrated or
removed. Setup does not change the selected engine or Transcript Cleanup mode.

Pinned components:

- Astral Python Build Standalone CPython 3.12.15, release 20261001,
  Apple arm64, SHA-256 `f1ee170bd7bb45bea526c4f9489b41f9c5d978c4cd7a08fd11809de56c39b736`.
- `fermion-research==0.2.7`, `mlx==0.31.1`, `mlx-audio==0.4.6`,
  `mlx-lm==0.31.1`. Every dependency is version and hash locked in
  `src-tauri/phonon/requirements.lock`. Pip accepts only wheels, uses its
  isolated mode, and ignores system pip configuration.
- Official model commit `ca1bef26bcd8ef4a7e16d0636d8a77bb25e298ee`.
  The 163,515,201-byte archive, configuration, manifest, and notices have
  size and SHA-256 pins. Fermion's unpacker verifies each archive member;
  setup also checks the resulting 177,438,361-byte container's digest.

The approximate installed size is 1.2 GB, including Python and the full
package dependency set. The model weights alone occupy about 178 MB.
Notices for the CC BY 4.0 model and Apache 2.0 runtime are installed; Python
and dependency distributions include their own licence files.

Setup has bounded child-process timeouts. A failed download discards its
staging file. A failed package install cannot mark the runtime installed.
Setup checks the real sandboxed, warmed worker before it writes a completion
stamp. The stamp includes hashes of the lockfile, adapter, and model revision;
a changed adapter requires another explicit Install action.

## Local worker

The app owns one persistent worker, started with its private Python and the
embedded adapter. It calls the official package's
`fermion._speech.engine_phonon2.load()` and
`transcribe_array_detailed()` APIs. These are internal APIs, so the package
version is pinned and checked. It uses Fermion's published `tdt16,dense16`
speed settings, checks that they loaded, and performs a one-second throwaway
decode before it reports ready. Long audio uses Fermion's pause-based
25–35-second windows above 35 seconds.

The worker has no HTTP server. A private inherited socket carries binary
mono 16 kHz float samples and the JSON result. No recording or transcript
file is created. macOS `sandbox-exec` denies all network operations, including
loopback. Offline environment flags are also set. There is no hub loader,
backend auto-selection, cloud fallback, prompt, context, or content telemetry.
Third-party stdout and stderr are discarded. Worker failures become fixed
messages that cannot quote a transcript.

The provider serializes setup, warm-up, decode, unload, and removal. Failed
or timed-out workers are killed and reaped, and the next request can start a
fresh worker. Startup and each complete request have a 180-second deadline.
Responses are capped at 1 MiB; audio is capped at 30 minutes. A process group
also stops pip children if setup times out. Catalogue shutdown prevents late
warm-ups from starting a new worker. Switching engines releases the model.

The result uses the same flat transcript shape as the previous Phonon
provider. Existing cleanup controls, including Basic, still run after ASR.
Confidence remains unreported. Existing app engine-selection rules remain
in force when a selected engine has no installed assets.

## Build and test

From this worktree:

```sh
npm run build -- --debug --bundles app
npm run dev
```

The first command only creates the worktree bundle. `npm run dev` signs and
opens that bundle with the project's existing `Slugtale Dev` identity.
Neither command installs over `/Applications/Slugtale.app`. Quit another
running Slugtale before testing the worktree bundle, then use its Install
button for Phonon-2.

Checks and the explicit local measurement harness:

```sh
npm test
npm run test:phonon-mlx
npm run test:whisper-build
npm run phonon:eval -- mlx /tmp/slugtale-test-models --install short.wav long.wav
npm run phonon:eval -- onnx /path/to/phonon-2 short.wav long.wav
```

Use `--show-text` only with a non-sensitive fixture. It prints the fixture
transcript. The harness reports fresh process load/warm-up, first decode,
and the median of three later decodes. ONNX uses the existing exact4x2
provider and its existing CPU thread settings. Run each engine separately
after builds have finished to avoid CPU competition.

## Measurements (3 October 2026)

Same Apple A18 Pro Mac, 8 GB RAM, macOS 26.6.2. Release harness, separate
engine processes, no builds running during the timed decodes. MLX uses its
network-denied worker; ONNX uses the unchanged exact4x2 CPU provider. Each
warm value is the median of three passes after the first decode.

| Test | MLX | ONNX exact4x2 |
|---|---:|---:|
| Fresh process load and warm-up, with cached files/kernels | 10.152 s | 1.585 s |
| First 3.7505 s clip decode after warm-up | 0.136 s | 0.586 s |
| Warm 3.7505 s clip decode | 0.111 s | 0.636 s |
| First 170.020 s clip decode | 3.466 s | 49.096 s |
| Warm 170.020 s clip decode | 3.602 s | 52.362 s |

MLX was 5.7× faster for the short clip and 14.5× faster for the long one.
The initial, uncached MLX worker start took 123.131 s while Rust builds were
also running. That is an observed first-use cost, not an idle cold comparison.
Setup pays that cost before marking the model ready. The 10.152 s figure
above is a later fresh worker, not a model already resident in that worker.

The short clip is a local Samantha system-voice recording of a made-up
14-word sentence. The long clip repeats it forty times with half-second
pauses. Both engines returned exactly the expected 14 and 560 words,
including punctuation and case. This verifies real local decoding and
long-window stitching. It does not establish accuracy on accents, noise,
names, or natural long conversations.

Fixture SHA-256:

- Short WAV: `108a77338bbf63012bda4a51c768686498001dc9c3f61f3ac9b5d817f6836976`
- Long WAV: `ee4f3dcd620a1d62558ae80f0b1253deef346410a683a8349fa30b6deca4e4df`

The tested runtime occupies 1,225,212,923 bytes. Setup and real transcription
were tested in `/tmp/slugtale-mlx-bench/`, without changing the installed app
or its settings. A socket connection under the worker's sandbox rules failed
with `PermissionError`, confirming OS-enforced network denial. macOS 14,
Intel Macs, Windows, and Linux were not tested on physical machines here;
their ONNX branch is retained and the combined ONNX/MLX build and tests pass.

## Official sources

- [Fermion's install and distribution](https://pypi.org/project/fermion-research/0.2.7/)
- [Fermion CLI and model layout](https://github.com/fermionresearch/phonon/blob/main/docs/cli.md)
- [Fermion's persistent worker and long-audio behaviour](https://github.com/fermionresearch/phonon/blob/main/docs/server.md)
- [Pinned model and notices](https://huggingface.co/FermionResearch/Phonon-2/tree/ca1bef26bcd8ef4a7e16d0636d8a77bb25e298ee)
- [Redistributable Python builds](https://github.com/astral-sh/python-build-standalone)
