# Slugtale verification map

Read this index before driving Slugtale. These files are the maintained source for user-facing verification. Read all files touched by a change; list each entry point as passed, failed, blocked, or not run. One convenient path cannot prove another listed entry point.

## Baseline preconditions

- Launch the isolated signed macOS app with `helpers/session.mjs launch` from the repository root, as described in [SKILL.md](../SKILL.md).
- Use only the bundle/identifier and evidence path printed by that run. Doctor must pass.
- Settings opens automatically for verification. Real defaults have no shortcut or model. No permissions, usage counts, or voice listener are enabled by the helper.
- Keep the user's installed app and its files separate. Do not double-drive global hotkeys, microphone, clipboard, or a shared text destination.
- The native CUA toolkit must be able to inspect the printed Settings title. No browser port or auth token is required.

## Driving conventions

- Use `cua.getApp` with the exact private bundle path, then native AX indices from the current tree.
- Each mapped name refers to the accessible label or visible control text. Source DOM IDs disambiguate controls; they are not native automation selectors.
- `click` means `await app.click(index)` with the fresh observed index. Follow each action with `await app.getAXState()`.
- `set value` means `await app.setValue(index, value)` for the observed field. If unavailable, focus it and type through native input. Do not inject events or call backend commands.
- Capture before/action/after and real file side effects. Restore a changed preference through the same UI where feasible, then clean up the private session.

## Proof and skip reporting

- Every proof needs feature ID, entry point, action log, AX text, screenshot, and result.
- Saves also need a `capture` state file. Downloads need completed files; speech needs destination output; opt-out needs the usage file removed.
- Record exact errors for blocked paths. No UI path is yet proven in this generated skill; see [validation.md](../validation.md).
- Automatically opening Settings does not prove tray navigation. Selecting a saved cleanup mode does not prove live transcript cleanup.

## Features

- [Settings, shortcut, and readiness](settings-and-readiness.md): tray entry, search, setup state, hotkey modes, permission views, and General.
- [Transcription and cleanup](transcription-and-cleanup.md): engine/model setup, speed, silence interval, second opinion, and cleanup preferences.
- [Dictation Bar appearance](dictation-bar.md): position, display, colour, preview, and live bar.
- [Dictation into another app](dictation.md): toggle/hold, stop/cancel, quiet-period insertion, and clipboard recovery.
- [Usage and typing speed](usage.md): opt-in counts, estimate, typing challenge, cancellation, redo, and opt-out deletion.
