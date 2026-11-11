# Loop2 - 2026-11-11 Sol pick Dictation Readiness only, stable interface, defer Settings File, defer Platform Adapter.
Worktree /Users/darrenbryant/.t3/worktrees/slugtale/loop-2 Branch arch/loop2-20261111-1012
Tasks: resolve engine choice via one internal decision, one settings snapshot per activation, no model loading, test preferred/fallback/no engine/missing model/hotkey vs voice, keep public types stable.
Parallel A readiness.rs transcription_engine.rs, B engine_catalogue.rs commands/settings.rs, C tests/settings-readiness.test.mjs LOOPS.md
Risks: Whisper fallback behavior test first, no cache across activations, local-only, no transcript in logs.

## Baseline evidence (2026-11-11, during A/B work)
Frontend: 192/192 tests pass (`scripts/run-node-tests.js`). settings-readiness.test.mjs: 10/10 pass.
Rust (`npm run test:rust`, `cargo test --lib`): COMPILE FAIL — A mid-edit. `src-tauri/src/transcription_engine.rs:847` syntax error (`let cases: &[...]` parsed as assignment; chained comparison). A owns readiness.rs + transcription_engine.rs (both dirty, uncommitted). B files (engine_catalogue.rs, commands/settings.rs) not yet dirty.
Contract check (settings-readiness.test.mjs vs src): items microphone/local_model/transcription_engine match Rust ReadinessItemId wire names; labels "Microphone" (test sample) vs Rust "Microphone permission" = sample-data drift, "Local model"/"Transcription engine" match; copy "Ready to dictate"/"Not ready to dictate", Linux polling message, and blocked-engine detail passthrough all match readiness-view.js + readiness-actions.js + readiness-copy.js.
#152: Rust side done (readiness.rs wire-name tests for ids + panes). JS side NOT done — test uses pane "transcription" only as sample data; no check that Settings-window pane names are Rust ReadinessPane wire names (shortcut/transcription/privacy/general). OPEN.
Final `npm test` (192 frontend + Rust) pending A/B finish. No src edits by evidence slice; no commit.
