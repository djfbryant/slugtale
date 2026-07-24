# Small Local ASR Models and Model Collaboration

Date: 2026-07-24  
Issue: slugtale-t6a

## Question

Could Slugtale improve dictation by adding local speech-recognition models beside
Whisper, especially Parakeet, and letting small models work together on the
8 GB Apple-silicon reference Mac?

## Conclusion

Yes, but a selective cascade is more promising than running a democratic
ensemble for every dictation.

The first macOS experiment should compare the existing Whisper `base.en` with:

1. Parakeet TDT-CTC 110M as a very small primary candidate;
2. Parakeet TDT v2 600M as the stronger English-only candidate;
3. Apple's `SpeechTranscriber` as a system-managed second opinion; and
4. Moonshine Small Streaming as the most interesting portable streaming
   candidate.

Whisper should remain the known-good baseline. Slugtale should initially select
one transcript, not merge text blindly. Run a second recognizer only when the
primary result is uncertain, empty, anomalous, or contains a word that matches a
user vocabulary poorly. If the two recognizers disagree, choose using calibrated
confidence and rules learned from a Slugtale dictation test set. Word voting is a
later experiment, not the starting design.

This approach is like asking a second listener only when the first says, “I
think that was *slug tail*.” Asking two people to repeat every perfectly clear
sentence costs time and still gives no answer when they disagree.

## Non-Negotiable Data Boundary

Every recognizer, selector, vocabulary specialist, and optional enhancement must
run entirely on the user's device. Captured audio, partial or final transcripts,
confidence data, alternatives, vocabulary, and application context must not
leave the local environment.

Model weights may be downloaded during an explicit setup or update flow, but
inference must continue to work with the network disabled and must have no
cloud/API fallback. Diagnostics and telemetry must never contain user content.
A platform service such as Apple's `SpeechTranscriber` is eligible only while
its transcription path is documented and verified as on-device.

## Reference Machine and Current Baseline

The reference machine is an Apple A18 Pro Mac with six CPU cores and 8 GB unified
memory. Slugtale currently ships Whisper `base.en` through whisper.cpp/whisper-rs.
The model file is 148 MB. The repository's existing warm benchmark measured
approximately 224–446 ms to finalize clean clips between 2.3 and 15.8 seconds,
depending on the clip and decode strategy. Those clips were synthesized, so they
are useful for latency but not a sufficient accuracy test.

Whisper `base.en` therefore already fits comfortably and has acceptable final
dictation latency. A replacement or partner has to improve real errors, not just
win a generic speech benchmark.

## Candidate Shortlist

| Candidate | Practical footprint | Strength | Main reservation | Recommendation |
|---|---:|---|---|---|
| Whisper `base.en` | 148 MB model | Mature, portable, already integrated | Older/smaller model; Whisper can hallucinate and miss names | Keep as baseline |
| Parakeet TDT-CTC 110M | About 228 MB of compiled Core ML weights for the complete pipeline | Small, fast; FluidAudio reports 3.01% LibriSpeech test-clean WER and 96.5x real time on M2 | Apple/Core ML path; clean-read-speech WER does not predict personal dictation | Benchmark first |
| Parakeet TDT v2 600M | Roughly 450–600 MB for the current compact Core ML components; upstream says at least 2 GB system RAM | English-only, punctuation, capitalization, timings, confidence; FluidAudio reports 2.1% LibriSpeech test-clean WER | Mac implementation is Swift/Core ML; keeping it and Whisper hot increases resident pressure | Benchmark first, load on demand initially |
| Parakeet TDT v3 600M | Similar class to v2 | 25 European languages and automatic language detection | v2 is the more relevant English-quality candidate; multilingual breadth adds no value to English-only v1 | Defer unless multilingual scope changes |
| Parakeet EOU 120M | About 225 MB for one compiled Core ML variant | True streaming and end-of-utterance detection; 320 ms configuration reports 4.88% LibriSpeech test-clean WER on M2 | Slugtale currently needs final transcription, not live partials; less accurate than batch Parakeet on the published clean set | Revisit with live preview |
| Moonshine Small Streaming | 123M parameters; shipped models are 8-bit ONNX/ORT | Portable C++/ONNX runtime across macOS, Windows, Linux, iOS, and Android; designed for short live utterances | Published Mac latency is from a different Mac; quantized accuracy is slightly worse than float | Best portable streaming spike |
| Distil-Whisper `small.en` | 336 MB GGML model, 166M parameters | Runs in whisper.cpp and is the lowest-effort higher-capacity experiment; MIT | It learned from Whisper and is likely to make correlated errors, so it is a weak ensemble partner | Test as a replacement, not as a voter |
| Apple `SpeechTranscriber` | System-managed model; Apple says it does not add to the app's runtime memory size | On-device, final/volatile results, alternatives, confidence, and time ranges; no model distribution work | macOS 26+/supported hardware only; Apple publishes no comparable WER and controls model updates | Excellent Mac-only second opinion |
| Canary 180M Flash | 182M parameters | Strong published English results, four languages, timestamps, CC BY 4.0 | Official deployment targets NeMo/NVIDIA; portable sherpa-onnx support is newer and less proven in a Rust desktop product | Watch/list B candidate |
| Qwen3-ASR 0.6B | FluidAudio Core ML INT8 variant is about 0.6 GB | 30 languages, automatic language detection, Apache 2.0 | FluidAudio labels it beta and reports roughly 60–80 ms per generated token | Not compelling for English dictation now |

Model sizes are not directly comparable to peak memory. Core ML, Metal, ONNX,
and whisper.cpp allocate differently, and Apple's system recognizer runs outside
the application's memory space. Peak resident memory and memory pressure must be
measured on the 8 GB machine.

## Is Parakeet Similar to Whisper?

Only at the product level: both turn audio into text locally.

Whisper is an encoder-decoder Transformer family that commonly processes padded
30-second windows. It has an unusually mature portable ecosystem, including
whisper.cpp. Parakeet uses FastConformer encoders with transducer or CTC decoding.
The TDT decoder predicts a token and how far to advance through the audio, which
helps it run very quickly. Parakeet also emits punctuation, capitalization,
timings, and confidence in the FluidAudio implementation.

On Apple hardware, the practical Parakeet path is FluidAudio: a Swift SDK using
Core ML and the Apple Neural Engine. It now has a Rust/Tauri wrapper, but this is
still a macOS-specific backend. The original NVIDIA model cards primarily target
NeMo on Linux/NVIDIA hardware. Moonshine and sherpa-onnx have cleaner portable
C/C++ or ONNX stories for the later Windows and Linux ports.

## Ways Models Can Work Together

### 1. Selective second opinion — recommended

Run one primary model. Escalate the captured audio to a second model when any of
these signals fire:

- low average or minimum word confidence;
- empty output despite detected speech;
- Whisper's low average log probability, high no-speech probability, repeated
  text, or implausible token timing;
- disagreement with a custom vocabulary or contact/project-name dictionary;
- unusually high disagreement between the primary transcript and one of its own
  alternatives.

Use a small calibration model or transparent rules to select the better complete
transcript. Do not compare raw confidence numbers from different engines until
they have been calibrated on the same recordings: `0.8` from Apple and `0.8`
from Parakeet do not necessarily mean the same thing.

Benefits: the normal path stays fast; the second model can be loaded only when
needed; and the app has an understandable fallback when one engine fails.

### 2. Main recognizer plus vocabulary specialist — recommended for names

FluidAudio already supports a useful two-model pattern: Parakeet TDT produces the
sentence while a 110M CTC model spots and rescores expected terms. Its published
Earnings22 experiment reports 99.3% keyword precision and 85.2% keyword recall.

This targets one of dictation's most irritating failure modes—project names,
people, libraries, and jargon—without asking a second general-purpose model to
rewrite the whole sentence. For Slugtale, a local vocabulary could contain terms
the user explicitly adds. It must not silently harvest document contents or
create dictation history.

### 3. ROVER/confusion-network voting — research later

NIST's ROVER algorithm aligns word sequences from multiple recognizers and votes
at each position. It can outperform every individual recognizer when their errors
are sufficiently different.

Two recognizers alone often produce a tie exactly where help is needed. A useful
system then needs a third hypothesis, calibrated word confidence, or a learned
quality estimator. The integration also needs word timings and per-word scores.
Slugtale currently flattens Whisper segments into one string, so the current ASR
result is too shallow for reliable word fusion.

ROVER should be tried only if an evaluation first shows a meaningful “oracle
gap”: across disagreement cases, each model is correct often enough that choosing
the better hypothesis would materially beat the best single model.

### 4. Local language-model judge — not recommended initially

A small local language model could choose or rewrite the hypotheses, but it may
prefer fluent text over what the user actually said. It also adds substantial
memory and makes mistakes harder to explain. This conflicts with Slugtale's
current deterministic-cleanup direction. It should remain a separate, explicit
enhancement feature if ever introduced.

## Proposed Experiment

### Phase A: establish whether another recognizer is actually better

Create an explicit, disposable evaluation corpus rather than using production
dictations or adding transcript history. Start with at least 100 labelled clips
spoken by the maintainer, covering:

- 1–3 second commands and 5–30 second dictation;
- quiet speech, fan/keyboard noise, and distance from the microphone;
- natural hesitations and self-corrections;
- numbers, punctuation, contractions, and capitalization;
- names and Slugtale/software terms;
- a silence/non-speech set to measure hallucination rate.

Compare Whisper `base.en`, Distil-Whisper `small.en`, Parakeet TDT-CTC 110M,
Parakeet TDT v2, Apple `SpeechTranscriber`, and Moonshine Small Streaming.

Measure normalized WER, proper-term recall, punctuation/capitalization error,
silence hallucinations, p50/p95 finalization latency, cold start, peak resident
memory, system memory pressure, and energy impact. Generic leaderboard WER is
only a prior; the maintainer's accent, microphone, and vocabulary decide the
product result.

Each candidate must also pass an offline boundary test: after model installation,
deny the app network access and verify transcription succeeds without attempted
remote fallback. Inspect diagnostics to confirm that no audio, transcript,
vocabulary, alternatives, or confidence content is written or transmitted.

### Phase B: measure complementarity

For every pair, record:

- how often they agree after normalization;
- each model's WER on disagreement cases;
- oracle WER if the better of the two could always be selected;
- whether confidence predicts which one is right; and
- the latency and memory cost of escalation.

Proceed to a cascade only if its measured selector closes a worthwhile portion
of the oracle gap. Proceed to word-level fusion only if complete-transcript
selection leaves a further material gap.

### Phase C: product shape

If the evidence is positive:

- retain Whisper as the portable fallback;
- add a platform-neutral ASR result containing segments/words, timings,
  confidence, alternatives, and engine metadata;
- add a macOS Parakeet or Apple adapter without putting Swift/Core ML concepts in
  the shared dictation workflow;
- keep only the default engine warm on 8 GB systems;
- label any multi-model mode as experimental until its thresholds are calibrated;
- show model downloads and disk use clearly; and
- log only timings, confidence summaries, and error codes—not audio or transcript
  text.

## Recommendation for Slugtale

Do not add a model picker or automatic multi-model voting yet. First build one
benchmark spike with the six candidates above.

The likely product outcome is:

- **macOS:** Parakeet TDT v2 or the 110M hybrid as primary, with Apple
  `SpeechTranscriber` or Whisper as an on-demand second opinion;
- **Windows/Linux:** Whisper or Distil-Whisper initially, with Moonshine as the
  most promising portable streaming alternative; and
- **technical vocabulary:** a lightweight vocabulary/CTC specialist, because it
  addresses specific errors more efficiently than a second full transcription.

That preserves Slugtale's local-only promise and gives the 8 GB Mac a realistic
quality path without turning every short dictation into three simultaneous model
runs.

## Licence Review

This is a technical licence review, not legal advice. Model weights and inference
code are separate works: an Apache-licensed runtime does not make the model it
loads Apache-licensed. Slugtale must audit the exact checkpoint and conversion,
not just the name of the model family.

| Candidate | Weight licence | Runtime licence | Slugtale position |
|---|---|---|---|
| Whisper `base.en` | MIT | whisper.cpp MIT; whisper-rs crates Unlicense | Low risk; already audited |
| Distil-Whisper `small.en` | MIT | Existing whisper.cpp stack | Low risk; retain MIT notices |
| Parakeet TDT v2, TDT v3, and TDT-CTC 110M | CC BY 4.0 | FluidAudio Apache 2.0 | Commercial use is allowed, but credit NVIDIA, link CC BY 4.0, identify the checkpoint, and say that Core ML conversion or quantization changed it |
| Parakeet EOU 120M | NVIDIA Open Model License | FluidAudio Apache 2.0 | Custom terms; do not adopt without a deliberate legal/product decision |
| Moonshine English models | MIT | Moonshine code MIT, with separately licensed third-party components | Suitable after auditing the exact packaged native libraries |
| Moonshine non-English models | Moonshine Community License, non-commercial | Same runtime | Exclude from Slugtale; non-commercial terms remove future distribution flexibility |
| Apple `SpeechTranscriber` | Proprietary system-managed Apple model | Apple Speech framework under Apple developer agreements | Do not bundle, extract, convert, or redistribute the model; use only through the documented API on supported Apple devices |
| Canary 180M Flash | CC BY 4.0 | NeMo or a separately audited portable runtime | Commercially usable with attribution and change notices |
| Qwen3-ASR 0.6B | Apache 2.0 | FluidAudio Apache 2.0 on Apple, or another separately audited runtime | Low risk; ship the Apache licence/NOTICE material and mark modifications |
| sherpa-onnx / ONNX Runtime | No weights included by this statement | Apache 2.0 / MIT | Suitable runtimes, but every loaded model keeps its own licence |

### Material caveats

- CC BY 4.0 permits commercial use and adaptation, but requires appropriate
  credit, a licence link, and an indication of changes. If Slugtale downloads a
  Core ML or quantized conversion at runtime, its model screen and release
  attribution bundle should still identify both the upstream model and the
  conversion. Do not imply endorsement.
- Some FluidInference model pages contain contradictory text: their metadata and
  upstream Parakeet model say CC BY 4.0 while a README sentence calls the
  conversion Apache 2.0. A conversion cannot be assumed to erase the upstream
  weight terms. Treat those Parakeet weights as CC BY 4.0 unless the rights holder
  publishes an unambiguous alternative grant.
- The NVIDIA Open Model License used by Parakeet EOU is commercially usable, but
  it is not the same permissive posture as MIT, Apache 2.0, or CC BY 4.0. It
  requires its licence and specified NVIDIA notice when redistributing weights,
  incorporates Trustworthy AI terms, protects guardrails, includes an indemnity,
  and allows NVIDIA to update the agreement for legal or regulatory reasons.
- Apple's current developer agreement restricts using output generated by an
  Apple model to train, fine-tune, or improve another AI model. Evaluating
  `SpeechTranscriber` against human references is different from training, but
  Slugtale should not train a learned transcript selector on Apple-generated
  hypotheses without a specific legal review. A fixed, non-learning fallback is
  the safer initial design.
- Runtime downloading avoids bundling large weights in Slugtale's installer, but
  it does not erase the model's use terms. Store the accepted licence identifier,
  source revision, conversion source, and digest beside every installed model so
  future audits and updates remain reproducible.

### Release requirements if a candidate ships

1. Pin the exact model repository revision and digest; do not rely on a mutable
   `main` URL.
2. Record separate SPDX-style identifiers for the runtime, original weights, and
   converted weights.
3. Add the required licence text, copyright/attribution, source URL, and change
   statement to `THIRD-PARTY-LICENSES.md` and the downloadable release
   attribution bundle.
4. Make model removal remove the weights, not the attribution record for a
   release that distributed them.
5. Reject GPL, AGPL, SSPL, BUSL, research-only, and non-commercial model terms
   unless the project makes an explicit exception before implementation.

## Sources

- [FluidAudio model guide](https://github.com/FluidInference/FluidAudio/blob/main/Documentation/Models.md)
- [FluidAudio ASR benchmarks](https://github.com/FluidInference/FluidAudio/blob/main/Documentation/Benchmarks.md)
- [FluidAudio ASR API](https://github.com/FluidInference/FluidAudio/blob/main/Documentation/API.md)
- [NVIDIA Parakeet TDT v3 model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
- [NVIDIA Parakeet Unified English model card](https://huggingface.co/nvidia/parakeet-unified-en-0.6b)
- [Moonshine Voice documentation and benchmarks](https://github.com/moonshine-ai/moonshine/blob/main/README.md)
- [Distil-Whisper `small.en` model card](https://huggingface.co/distil-whisper/distil-small.en)
- [Apple SpeechAnalyzer session](https://developer.apple.com/videos/play/wwdc2025/277/)
- [Apple `SpeechTranscriber.Result` documentation](https://developer.apple.com/documentation/speech/speechtranscriber/result)
- [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx)
- [NVIDIA Canary 180M Flash model card](https://huggingface.co/nvidia/canary-180m-flash)
- [Qwen3-ASR 0.6B model card](https://huggingface.co/Qwen/Qwen3-ASR-0.6B)
- [NIST ROVER paper](https://www.nist.gov/publications/post-processing-system-yield-reduced-word-error-rates-recognizer-output-voting-error)
- [CC BY 4.0 licence summary](https://creativecommons.org/licenses/by/4.0/)
- [NVIDIA Open Model License](https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/)
- [Apple Developer Program License Agreement](https://developer.apple.com/support/terms/apple-developer-program-license-agreement/)
