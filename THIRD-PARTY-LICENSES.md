# Third-Party Licenses

Slugtale itself is MIT-licensed (see [LICENSE](LICENSE)). This document records the
licenses of the software Slugtale depends on and what they mean for **commercial
use and distribution** of Slugtale binaries.

Audited 2026-07-02 against `src-tauri/Cargo.lock` with the `local-whisper-runtime`
feature enabled (the configuration `npm run build` and `npm run dev` actually ship).

## Verdict for commercial use

**Every dependency permits commercial use, sale, and closed-source distribution.**
There is no GPL or other whole-program copyleft anywhere in the dependency tree.
The obligations that do exist are:

1. **Attribution** — MIT / BSD / ISC / Apache-2.0 licenses require shipping the
   dependency copyright notices and license texts with distributed binaries.
   Generate this at release time (e.g. `cargo install cargo-about && cargo about generate`)
   and attach it to the GitHub Release alongside the artifacts (ADR-0022).
2. **MPL-2.0 file-level copyleft** — five transitive crates (below) are MPL-2.0.
   This does **not** affect Slugtale's own code or require open-sourcing anything,
   as long as those crates' files are not modified. If they ever were modified,
   only the modified files' source must be made available.

## Speech stack (the parts that make Slugtale a dictation app)

| Component | License | Notes |
|---|---|---|
| `whisper-rs`, `whisper-rs-sys` (0.16 / 0.15) | Unlicense | Public-domain dedication; no obligations at all. |
| whisper.cpp + ggml (compiled in via `whisper-rs-sys`) | MIT | Copyright Georgi Gerganov. Include the MIT notice in binary attributions. |
| Whisper model weights `ggml-base.en.bin` (downloaded at runtime from `huggingface.co/ggerganov/whisper.cpp`) | MIT | OpenAI released the Whisper models under MIT; the ggml conversions keep that license. Commercial use of the model and its transcriptions is permitted. The model is downloaded by the user at runtime, not redistributed in the app bundle, which further reduces obligations. |
| `cpal` 0.18 (audio capture) | Apache-2.0 | Attribution; include any NOTICE file. |

## Parakeet Transcription Engine (optional `local-parakeet-runtime` feature)

Audited 2026-07-25 for slugtale-vjs.1. None of this is compiled into the default
build; it applies only to releases built with `local-parakeet-runtime` (or
`local-parakeet-runtime-coreml`).

| Component | License | Notes |
|---|---|---|
| `parakeet-rs` 0.3.6 | MIT OR Apache-2.0 | Choose MIT; attribution only. |
| `ort`, `ort-sys` 2.0.0-rc.12 (ONNX Runtime bindings) | MIT OR Apache-2.0 | Choose MIT; attribution only. |
| ONNX Runtime itself (prebuilt binary linked in by `ort-sys` at build time) | MIT | Copyright Microsoft. Include the MIT notice in binary attributions. Note the binary is fetched at **build** time, so it is a release-engineering input, not a runtime download. |
| `tokenizers` 0.23, `hound` 3.5 | Apache-2.0 | Attribution; include any NOTICE file. |
| `ndarray`, `rustfft`, `realfft`, `onig`, `onig_sys`, `eyre` and the rest of the Parakeet subtree | MIT and/or Apache-2.0 | All permissive; attribution only. `onig_sys` vendors Oniguruma (BSD-2-Clause) — keep its notice. |
| **Parakeet TDT v2 0.6B model weights** — `nvidia/parakeet-tdt-0.6b-v2`, installed at runtime as the ONNX int8 export `istupakov/parakeet-tdt-0.6b-v2-onnx` pinned at commit `0bbb45a3365852604aef28b538a8f066f4ccaa85` | **CC BY 4.0** | The one dependency with a real, ongoing obligation (see below). Commercial use and commercial use of the transcriptions are permitted. The weights are downloaded by the user through an explicit Settings action and are never bundled or redistributed in the app. |

### What CC BY 4.0 obliges Slugtale to do

CC BY 4.0 is an attribution licence, not copyleft: it does not touch Slugtale's
own source or the transcriptions the model produces. It requires three things,
all of which are carried in `EngineMetadata` and rendered by Settings so they are
visible to the user rather than buried in a file nobody opens:

1. **Credit NVIDIA** — "Speech recognition by NVIDIA Parakeet TDT 0.6B v2
   (© NVIDIA Corporation), used under CC BY 4.0."
2. **Link the licence** — <https://creativecommons.org/licenses/by/4.0/>.
3. **State the changes** — the installed artefact is not NVIDIA's original NeMo
   checkpoint. It was exported to ONNX and quantised to int8 upstream, and
   Slugtale installs those artefacts unmodified; it does not train, fine-tune, or
   otherwise alter the weights.

The strings are single-sourced in `src-tauri/src/parakeet.rs`
(`PARAKEET_ATTRIBUTION`, `PARAKEET_LICENSE_URL`, `PARAKEET_MODIFICATIONS`) and
covered by tests, so a change there cannot silently drop the credit.

Deliberately **not** used: NVIDIA's Parakeet EOU 120M model, which ships under
the NVIDIA Open Model License rather than CC BY 4.0 and would need a separate
review (slugtale-vjs).

## Apple SpeechTranscriber engine (optional `apple-speech-runtime` feature)

Audited 2026-07-25 for slugtale-vjs.2. None of this is compiled into the default
build, and none of it exists at all outside macOS; it applies only to macOS
releases built with `apple-speech-runtime`.

| Component | License | Notes |
|---|---|---|
| `Speech.framework` (`SpeechAnalyzer`, `SpeechTranscriber`, `AssetInventory`), `AVFoundation`, `Foundation`, `CoreMedia` | Apple OS components | Dynamically linked from the user's own macOS at runtime; nothing is redistributed, so no license obligation. Same footing as the existing `pbcopy` / `osascript` / system-sound usage below. |
| Swift runtime (`/usr/lib/swift/libswiftCore.dylib` and friends) | Apple OS components | Also dynamically linked from macOS. Slugtale ships **no** copy of the Swift standard library: the bridge targets macOS 13, which is above the back-deployment cliff, so no `libswift_Concurrency` or `swiftCompatibility*` archives are bundled. |
| Swift compiler (`xcrun swiftc`, via `src-tauri/build.rs`) | Apache-2.0 with LLVM exception | Build-time only; never distributed with the app. |
| **Apple speech model assets** | Apple OS components, **system-managed** | The obligation here is a negative one (see below). |

### What Apple's terms oblige Slugtale to do

The speech models are downloaded, stored, versioned, and deleted by macOS under
the macOS Software Licence Agreement. Slugtale's obligations are things it must
*not* do, and each one is enforced in code rather than left to good intentions:

1. **Do not extract, bundle, or redistribute the assets.** Slugtale never reads
   an asset file. Installation goes through `AssetInventory`'s own request
   object, and `EngineMetadata` reports `system_managed: true` with no
   `source_url` and no `approximate_bytes`, so Settings cannot imply the app
   ships Apple's model.
2. **Do not fall back to remote dictation.** `SpeechAnalyzer` has no server mode,
   and the bridge refuses to transcribe unless macOS reports the locale's assets
   as installed. The legacy `SFSpeechRecognizer`, which *does* have a server
   path, is not used and not linked.
3. **Do not use the output to train, fine-tune, or improve another model.**
   Transcripts, alternatives, and confidence go in-process to the Second Opinion
   router and the Text Insertion path and nowhere else; a test forbids print and
   logging calls in both halves of the engine.

Apple asks for no credit for use of a system API, so `EngineMetadata.attribution`
is `None` — inventing one would misstate the relationship as much as omitting a
required one would.

## App shell and libraries

| Component | License | Notes |
|---|---|---|
| Tauri 2 (`tauri`, `tauri-build`, plugins `global-shortcut`, `autostart`, `wry`, `tao`) | Apache-2.0 OR MIT | Choose MIT; attribution only. |
| `serde`, `serde_json`, `ureq` (model download), most of the tree (~380 crates) | MIT and/or Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, Unlicense, CDLA-Permissive-2.0 (webpki-roots data) | All permissive; attribution only. |
| `objc2-*`, `block2` (macOS bindings) | MIT / (Zlib OR Apache-2.0 OR MIT) | Permissive. |
| `rustls`, `ring`, `webpki-roots` (TLS for model download) | Apache/ISC/MIT mix | Permissive; `ring` requires keeping its license text in attributions. |

## Copyleft-adjacent items (checked, all safe)

| Component | License | Why it is safe commercially |
|---|---|---|
| `cssparser`, `cssparser-macros`, `selectors`, `dtoa-short`, `option-ext` (transitive via Tauri) | MPL-2.0 | File-level copyleft only. Linking them into a proprietary app is expressly permitted; obligations trigger only if their own source files are modified. |
| `r-efi` | MIT OR Apache-2.0 OR LGPL-2.1+ | Tri-licensed; Slugtale elects MIT. |
| GTK / WebKitGTK / glib system libraries (Linux builds only) | LGPL | The Rust binding crates are MIT and link the system libraries **dynamically**; they are not bundled with the app. Dynamic linking to LGPL system libraries does not restrict commercial or closed-source distribution. Not applicable to macOS/Windows builds. |

## Assets and tooling

| Component | License | Notes |
|---|---|---|
| Lucide icons (SVG path data inlined in `src/index.html`) | ISC | Attribution in this file satisfies it; commercial use fine. |
| Inter font | Referenced by `font-family` name only, not bundled | No obligation. If ever bundled, Inter is SIL OFL 1.1 (commercial use fine; the font alone may not be sold). |
| macOS system sounds (`Tink.aiff`, `Pop.aiff`), `afplay`, `pbcopy`, `osascript`, `open` | Apple OS components | Invoked on the user's machine at runtime; nothing is redistributed, so no license obligation. |
| `@tauri-apps/cli` (npm devDependency) | MIT OR Apache-2.0 | Build-time only; never distributed with the app. |

## Release checklist (licensing)

- [ ] Generate an attribution bundle (`cargo about generate` or `cargo license`) for the
      exact feature set being shipped and attach it to the GitHub Release.
- [ ] Keep the Lucide ISC notice (already documented here and in `src/index.html`).
- [ ] Re-run the audit when adding dependencies; anything reporting GPL, AGPL, SSPL,
      BUSL, or "non-commercial" terms needs a decision before merging.
