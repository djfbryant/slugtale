# Developer-Run First

> **Superseded by ADR-0022 (Zero-Cost Distribution via GitHub Releases).** Slugtale now targets packaged, downloadable executables. The packaging, code signing, and notarization work this ADR deferred is now scoped under the macOS distribution epic and ADR-0022.

Slugtale's first version will target developer-run builds rather than packaged installers. This keeps the initial implementation focused on the dictation workflow while leaving Tauri packaging, code signing, notarization, and Windows installer trust as later distribution work.
