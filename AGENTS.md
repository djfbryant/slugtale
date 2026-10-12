# Agent instructions

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

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

## Project Checks

Use npm scripts for checks instead of bare `cargo ...` commands:

```bash
npm test                    # frontend tests, then Rust unit tests
npm run test:rust           # Rust-only tests via scripts/run-cargo.js
npm run test:whisper-build  # compile the Whisper runtime (CI runs this on Windows)
```

Frontend tests live in `tests/` as `*.test.mjs`; `npm test` discovers them from
that directory, so a test placed elsewhere is silently skipped. Rust unit tests
live in a `#[cfg(test)] mod tests` in the module they cover — a module gated to
one platform must test from inside itself, since `lib.rs` cannot reach it.

The runner looks for Cargo on `PATH`, then at `$HOME/.cargo/bin/cargo`. If
Cargo is installed somewhere else, set `CARGO=/path/to/cargo`.

Every cargo invocation through these scripts has a hard wall-clock cap
(default 10 minutes) that kills the entire cargo process tree, so a hung
test or build cannot exhaust the machine. Override per run with
`SLUGTALE_CARGO_TIMEOUT=<seconds>`, or `SLUGTALE_CARGO_TIMEOUT=0` to disable.
