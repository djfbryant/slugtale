# Usage and typing speed

Users can opt into local counts, set or measure typing speed, and see estimated time saved without storing transcript history.

## Sub-features

- `usage-opt-in`: counts are off by default and can be enabled.
- `usage-estimate`: whole-number speed estimate, validation, and Clear.
- `usage-challenge`: three typing challenges, resume, close, and redo.
- `usage-counts`: real dictation changes Today, This week, and all-time counts.
- `usage-opt-out`: confirmation/cancel and real usage-file deletion.

## How to get to it (user POV)

- Open Settings, then `Usage`, or search `typing`/`usage`.
- Select `Measure my typing speed`, `Continue typing challenge (… of 3)`, or `Redo` when offered.
- Toggle `Store usage counts`. Turning it off opens an in-app confirmation.

## Driving it with native CUA

Preconditions: doctor passes, private disposable settings selected. Estimates/challenges need no mic/model. Counts need the real speech path. Never test deletion on the installed app.

- **Defaults + estimate.** Click `Usage` (`rail-usage`), capture storing off and `Not measured yet.` Set `Typing speed estimate in words per minute` (`usage-estimate-input`) to `40`, click `Save`, and capture `40 words per minute, your estimate.` plus persisted typing baseline in the real settings file. Enter `40.5` and require `Enter a whole number of words per minute.` without overwriting the previous stored estimate. Use `Clear` and capture the empty estimate/default baseline.
- **Challenge.** Click `Measure my typing speed`. Require the real `Slugtale Typing Challenge` window, `Challenge 1 of 3`, and `Type the passage here`. Type the displayed passage through native input; capture timer start, the result after thirty seconds, and real baseline state. Use `Next challenge` for all three; Settings must show measured speed and hide the editable estimate. Capture intermediate completion counts as well as the final result. `Close` and title-bar close are separate cancellation paths; neither must leave dictation disabled. Reopen through Continue when offered. Do not paste text at artificial speed and present it as a human typing baseline.
- **Redo.** Click `Redo` in Usage, capture the confirmation, cancel once, then repeat and accept using the visible confirmation control. Require baseline reset and a new challenge window. Capture persisted baseline state.
- **Counts.** Enable `Store usage counts` (`usage-store-toggle`). Drive one real scratch dictation, then capture visible changed counts and inspect `dataDir/usage.json`; require count records with no audio/transcript content. Zero cards after enabling alone do not prove recording a count.
- **Opt-out.** Toggle storing off. First click `Cancel` in the confirmation and require counts/file retained. Repeat and click the fresh confirmation accept button (`usage-confirm-accept`, read its actual label). Require storing off, cleared visible counts, persisted `store_usage: false`, and absence of `usage.json`. Capture before and after.
- **Search entry.** Search `typing`, edit/save an estimate from results, and require the same visible and persisted state. Restore/clear the estimate through UI afterward.

## Gotchas

- Store counts is optional; Time saved depends on a baseline and cannot be proved by setting only an estimate.
- Turning storing off deletes counts. Use only disposable verification data.
- A completed measurement disables the estimate. Do not use a setter to bypass it.
- The typing timer starts with input, not when its window opens. Capture real elapsed time and intermediate state.
