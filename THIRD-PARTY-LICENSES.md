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
