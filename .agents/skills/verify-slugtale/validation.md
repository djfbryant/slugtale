# Validation record

Status: **draft; native UI proof blocked**. Remaining work is tracked in [GitHub issue #36](https://github.com/djfbryant/slugtale/issues/36).

On 2026-10-12 the helper ran on macOS in this checkout. Final run: `evidence/2026-10-12T06-46-24.980Z-7b4953f2/`.

- Launch built the current source and opened a private signed Slugtale 0.1.1 bundle. The build log records the source checkout and output bundle.
- Doctor passed process/start identity, source and binary hashes, version, identifier, signature, and owned data checks.
- Capture wrote real local state. No UI save or dictation was claimed.
- Native `cua.getApp` returned `Computer Use server error -10005: cgWindowNotFound` for the test bundle identifier and full bundle path. The same error occurred for Finder. A known T3 Code window check returned `Sky Computer Use request timed out` after 120 seconds.
- Cleanup stopped the private process, removed private scratch/data/build bundle, and preserved evidence. A second cleanup run also passed. File checks confirmed the state capture, session, launch config, and logs survived; private paths and the launch lock were removed; the user's installed app remained running.
- Helper syntax, feature headings, local document links, and executable permissions passed. No app source changed, so the full application test suite was not run.

Earlier failed runs remain as diagnostic evidence. Launch Services uses canonical `/private/var` executable paths; the helper now resolves its scratch path before tracking the process. The failed pre-fix process was stopped by its exact PID after checking its full command path.

Next proof: run the skill's Launch, Doctor, `transcription-cleanup` UI recipe, Capture, and Cleanup. Require screenshots and AX/action logs plus persisted `clean-dictation` and restored `basic`. Until that passes, do not call this skill fully verified.

The skill and evidence moved from `.cursor/skills` to `.agents/skills` during the GitHub issue migration. Launch instructions and session evidence-directory paths were updated. Helper syntax, feature links, and repeated cleanup at the new path passed. Earlier logs keep their historical paths.
