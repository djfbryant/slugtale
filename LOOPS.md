# Loop3 - 2026-11-11 Sol pick Settings File + Local Log/Timing
Branch arch/loop3-20261111-1041 Base 5e516d2
Tasks: Settings: remove AppFiles::save_settings, move to update_settings, lock/saves/rollback inside. Logs: box each sink once inside owner, replace .erased() with cloned handles, keep typed calls, remove extra locks.
Parallel A settings change + tests, B log handles + callers, C review + npm test.
Risks: settings rollback/lock order, log enable flag copy rule, local-only, no audio/transcript history, OS adapters visible.

## Baseline evidence (2026-11-11, before A/B changes, worktree clean except A starting app_files.rs)
Frontend (`node scripts/run-node-tests.js` / `npm test` frontend part): 192/192 pass, 0 fail, duration ~4.7s.
Rust (`npm run test:rust`, `cargo test --lib`): 583 passed, 0 failed, 0 ignored, finished ~5.09s.
Total `npm test` (frontend + Rust): 775 pass (192+583), 0 fail.
Privacy/caller baseline review (read-only, pre-A/B): `AppFiles::save_settings` still present (app_files.rs) with callers in app_files tests + local_model/mod.rs:317 — A must migrate all to `update_settings` inside lock. `.erased()` still present at app_files.rs:121, commands/dictation.rs:179, dictation_host.rs:827, engine_catalogue.rs:920,962 — B must replace with cloned typed handles, box sink once inside owner. No src edits by evidence slice; no commit.
