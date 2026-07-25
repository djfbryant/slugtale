# Local ASR evaluation rig

Date: 2026-07-25 · Issue: slugtale-9dv.1

`src-tauri/examples/asr_research.rs` is a development-only recorder, adapter
runner, and deterministic scorer. It does not participate in Slugtale's app
build or Dictation Workflow. It reuses `CpalAudioRecorder`, so recordings take
the same microphone/downmix/resampling path as real dictation, but all research
data goes to one directory the maintainer explicitly chooses.

## The local-only boundary

The research directory contains sensitive material: the manifest's reference
text, recorded voice clips, and each engine's hypotheses and confidence values.
The tool refuses a relative path, the Slugtale repository (or anything under
it), and known Slugtale application-data/history directories. WAV paths in the
manifest must be relative and cannot escape through `..` or symlinks.

The tool does not write to settings, model storage, Dictation History, the Local
Diagnostic Log, telemetry, or the network. A custom adapter is a separate
executable and must honour the same boundary. Run the offline check below before
trusting one. Terminal output from `record` necessarily shows the current
reference prompt; terminal output from `run` contains identities and counts
only. `score` is the only command intended for publication, and its JSON
contains aggregates rather than references or hypotheses.

Do not commit the research directory. The path checks make that mistake hard,
but the directory should still be treated like any other local file containing
recorded speech.

## Create and record a corpus

Copy the non-sensitive example plan out of the repository, edit it there, and
create the research directory. Keep `schema_version` at `1`; clip IDs and WAV
paths must be unique.

```sh
cp -f docs/research/asr-evaluation-manifest.example.json /tmp/slugtale-asr-plan.json
# Edit /tmp/slugtale-asr-plan.json and expand it to the desired corpus.

npm run asr:research -- init \
  --research-dir /absolute/private/path/slugtale-asr-corpus \
  --manifest /tmp/slugtale-asr-plan.json
```

The parent benchmark requires at least 100 labelled clips covering short and
long dictation, clean and noisy speech, hesitations, numbers, punctuation,
names/jargon, and silence. The eight-entry example is deliberately only a smoke
corpus. Duplicate and adapt its entries before the real benchmark.

Record the first missing clip. Enter starts and stops capture; after capture,
choose keep, retry, delete, or quit. Running the same command later resumes at
the first missing clip.

```sh
npm run asr:research -- record \
  --research-dir /absolute/private/path/slugtale-asr-corpus
```

Re-record or delete a specific clip without changing its manifest entry:

```sh
npm run asr:research -- record \
  --research-dir /absolute/private/path/slugtale-asr-corpus \
  --clip 004-numbers-clean --replace

npm run asr:research -- delete \
  --research-dir /absolute/private/path/slugtale-asr-corpus \
  --clip 004-numbers-clean
```

Validate the manifest and every present recording. Saved clips must be mono,
16 kHz, IEEE float32 WAV. Missing clips are reported as resumable work; corrupt
or wrongly formatted clips fail validation.

```sh
npm run asr:research -- validate \
  --research-dir /absolute/private/path/slugtale-asr-corpus
```

## Adapter and result protocol (version 1)

`run` starts one adapter process and sends one JSON object per line on stdin.
The process replies with one JSON object per line on stdout, in the same order.
Keeping one process alive across the corpus makes the first decode cold and
later decodes warm. It also gives every engine exactly the same WAV files.

Request fields are `schema_version`, `clip_id`, and the absolute local
`wav_path`. A successful result contains:

- `schema_version`, matching `1`;
- the same `clip_id`;
- `engine` with stable `engine`, `model`, and optional `revision` identities;
- `hypothesis`, plus optional whole-result `confidence` in `0..1`;
- total `latency_ms` and an optional `timings_ms` object.

A failed result replaces `hypothesis` with `error: { code, detail }`. Error
details must be non-content: describe missing assets, invalid audio, or a local
runtime failure without quoting any reference or partial hypothesis. Adapter
stderr is discarded so an accidental debug print cannot enter a report. Full
result files are stored under `<research-dir>/runs/` and remain sensitive.

Any entirely-local recogniser can plug in through this protocol:

```sh
npm run asr:research -- run \
  --research-dir /absolute/private/path/slugtale-asr-corpus \
  --run-id candidate-model-revision \
  --adapter /absolute/path/to/local-adapter \
  --adapter-arg --model \
  --adapter-arg /absolute/path/to/local/model
```

## Whisper baseline

The bundled baseline adapter uses Slugtale's real `LocalWhisperRuntime`. Build
it with the local runtime feature and run it end to end over the same corpus:

```sh
npm run asr:research:whisper -- run-whisper \
  --research-dir /absolute/private/path/slugtale-asr-corpus \
  --run-id whisper-base-en \
  --model "/absolute/path/to/ggml-base.en.bin" \
  --model-id base.en \
  --revision whisper.cpp-local
```

The adapter reports no confidence because the production Whisper boundary does
not expose a calibrated score. That is represented as an absent value, not a
made-up number.

## Score comparable runs

```sh
npm run asr:research -- score \
  --research-dir /absolute/private/path/slugtale-asr-corpus \
  --run whisper-base-en \
  --run candidate-model-revision \
  > /tmp/slugtale-asr-aggregate.json
```

The deterministic report contains per-engine normalized micro-WER, proper-term
recall, punctuation and capitalization sequence accuracy, silence hallucination
rate, latency p50/p95, and confidence expected calibration error when the
engine supplied confidence. For every engine pair it contains normalized exact
agreement, disagreement count and per-engine disagreement WER, plus oracle WER
(the better whole hypothesis per clip). The scorer never merges words from two
hypotheses and never emits clip-level content.

The first clip's adapter latency includes model loading (cold); later clips use
the same process (warm). Preserve clip order between runs. For an explicit cold
distribution, run one-clip manifests in separate processes.

## Offline and resource checks

Install model weights during an explicit setup step, then build the example and
deny networking to the already-built executable. On macOS:

```sh
npm run asr:research:build

sandbox-exec -p '(version 1)(deny network*)(allow default)' \
  src-tauri/target/release/examples/asr_research run-whisper \
  --research-dir /absolute/private/path/slugtale-asr-corpus \
  --run-id whisper-offline \
  --model "/absolute/path/to/ggml-base.en.bin"
```

On Linux, use `unshare --net --map-root-user -- <command>` or
`firejail --net=none -- <command>`. A successful run with networking denied is
the required evidence; there is no cloud fallback.

Measure each engine in a fresh process because peak RSS is process-wide. Useful
hooks are `/usr/bin/time -l <command>` on macOS, `/usr/bin/time -v <command>` on
Linux, `sudo powermetrics --samplers thermal,cpu_power -i 1000` on macOS, and
`sudo turbostat --interval 1` on Linux. Store raw resource logs inside the
research directory if they contain paths or process context. Publish only the
summary numbers.

## Retention, archival, and deletion

Archive only when the corpus is still needed for a reproducible local rerun.
Use an encrypted local volume or encrypted archive, document the engine/model
revisions alongside it, and never place the archive in Git or Slugtale storage.
Result JSON is as sensitive as the WAV files because it contains hypotheses.

To delete, first resolve and inspect the exact directory, then remove that one
explicit path using a non-interactive command. Never substitute the repository,
home directory, an unresolved variable, or a glob:

```sh
realpath /absolute/private/path/slugtale-asr-corpus
rm -rf /absolute/private/path/slugtale-asr-corpus
```

Deletion is irreversible unless the directory is on a volume with recoverable
snapshots or has an encrypted backup.
