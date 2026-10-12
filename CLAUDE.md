# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

## Issue tracking

Use [GitHub Issues](https://github.com/djfbryant/slugtale/issues) for all project task tracking. Read the relevant issue before starting. Create issues for remaining work, update progress in the issue, and close it only when its acceptance checks pass.

```sh
gh issue list --repo djfbryant/slugtale
gh issue view "$SLUGTALE_ISSUE_NUMBER" --repo djfbryant/slugtale
gh issue create --repo djfbryant/slugtale --title "Task title" --body-file /tmp/slugtale-issue.md
gh issue close "$SLUGTALE_ISSUE_NUMBER" --repo djfbryant/slugtale --reason completed
```

Set `SLUGTALE_ISSUE_NUMBER` to the actual issue number returned by GitHub. Preserve dependencies and parent/child links in issue bodies. Use `priority:P0` through `priority:P4` and the existing type/status labels when needed. Keep durable technical facts in project documentation linked from the issue.

Do not commit or push unless the user asks. When handing off, report changed files, checks, issue status, and any unfinished work.

## Desktop verification

Before verifying a user-facing Slugtale change, read [.agents/skills/verify-slugtale/SKILL.md](.agents/skills/verify-slugtale/SKILL.md) and the relevant feature guides. The skill is shared across agents in this repository. It remains a draft until its blocked native UI proof passes. Report that limit accurately.



## Build & Test

Requires Rust stable (install via `rustup`). The Tauri CLI is installed as an npm devDependency.
Use npm scripts for project checks; they find Cargo from `PATH`, `CARGO`, or the standard rustup location.

```bash
npm install                 # install Tauri CLI
npm test                    # frontend tests, then Rust unit tests without app launch
npm run test:rust           # Rust-only tests via scripts/run-cargo.js
npm run test:whisper-build  # compile the Whisper runtime (CI runs this on Windows)
npm run dev                 # build, sign, and open the development app on macOS
npm run build               # build a release bundle
```

Prerequisites on macOS: Xcode Command Line Tools (`xcode-select --install`).

## Architecture Overview

Slugtale is a Rust/Tauri v2 tray/menu-bar resident app (ADR-0007, ADR-0008).

```
src/                    Frontend (plain HTML settings window)
src-tauri/
  src/
    lib.rs              App logic and Rust unit tests (no Tauri command macros here)
    main.rs             Tauri entry point — wires Builder, managed state, window events,
                        and the one `generate_handler!` list
    commands/           The Tauri command adapters, grouped by domain
  tauri.conf.json       App config: settings window hidden by default; bundling enabled
  capabilities/         Tauri v2 permission manifests
  icons/icon.png        Generated tray/app icon
```

Key design rule: `lib.rs` contains only plain Rust — no `#[tauri::command]` macros. Command wrappers live in `src/commands/`, one module per domain (settings, dictation, engines, usage, updates, platform). `lib.rs` re-exports each of its modules with `pub use`, so the domain modules can be split, renamed and moved without breaking a `slugtale_lib::*` call site. This keeps the library fully unit-testable without a running Tauri runtime: only the adapters in `commands/` and the small shell modules the binary owns (`hotkey_registration.rs`, `voice_activation.rs`, `dictation_bar_window.rs`) touch the Tauri runtime.

## Conventions & Patterns

- Domain vocabulary is defined in `CONTEXT.md` — use it in code and test names.
- Platform-specific OS behavior lives behind a **Platform Adapter** boundary (ADR-0021). This includes hotkeys, permissions, audio, notifications, and file-manager reveal. Any spawned OS process (`open`, `explorer`, `xdg-open`, `afplay`, `osascript`) belongs in `macos.rs`, `windows.rs`, or `linux.rs`. Domain modules keep the decision logic; the platform modules answer the same questions under the same names. The command layer asks those questions through the `PlatformAdapter` trait and the one `CurrentPlatform` this build carries (`commands/platform.rs`), so a command never writes a `#[cfg]` arm of its own — a new question is one trait method rather than one arm per platform (see `commands/platform.rs`, `recording_feedback.rs`, `local_model/mod.rs`).
- Rust unit tests live in a `#[cfg(test)] mod tests` in the module they cover, preferring that module's public interface. `lib.rs` keeps only the tests for behavior it owns itself. A module gated to one platform (`macos.rs`, `windows.rs`, `linux.rs`) must test from inside itself — its code does not exist on the other platforms, so `lib.rs` cannot reach it — and may reach a private policy function there rather than leave it uncovered.
- Frontend tests live in `tests/` as `*.test.mjs`. `npm test` discovers them from that directory, so a test placed elsewhere is silently skipped.
- Tauri commands in `commands/` are thin adapters that delegate to `lib.rs` functions: they translate the wire types and reach the Tauri runtime, and the rules stay in the library.
