# Settings, shortcut, and readiness

Settings shows what must be fixed before dictation and lets a user find preferences, record a shortcut, and inspect access and app options.

## Sub-features

- `settings-tray`: open Settings from the menu bar.
- `settings-search`: matching, empty results, and clearing search.
- `settings-readiness`: loaded overall status, section warning counts, and permission rows.
- `shortcut-record`: Record/Change, cancel capture, clear, Toggle, and Hold.
- `settings-general`: system appearance, login preference, and manual update check.

## How to get to it (user POV)

- Select Slugtale's menu bar icon, then `Settings…`.
- Select a sidebar section or enter words in `Search settings`.
- Select `Shortcut`, `Privacy & access`, or `General`. Setup warnings link to OS access settings.

## Driving it with native CUA

Preconditions: doctor passes; exact private window selected; no global dictation drive is in progress.

- **Tray entry.** Inspect the private app's tray/menu AX state, click its icon, then the fresh `Settings…` menu item. Capture the printed window title. If the tray is absent from the tree, use a fresh screenshot to identify the private icon; with two identical icons, do not guess. Report blocked if identity cannot be established.
- **Search.** Set the `Search settings` field (`settings-search`) to `pause`. Require a `Results` heading and the `Insert after quiet` row. Set it to `zzzz-no-setting` and require `No setting matches.` Clear the field with `setValue(index, '')`; sidebar panes must return. This is read-only and needs no save.
- **Readiness.** Click `Privacy & access` (`rail-privacy`), inspect `Microphone` and `Text insertion` rows, and compare warnings with `Not ready to dictate`. Fresh defaults must not falsely report ready. Access grants are separate user-controlled OS steps.
- **Shortcut.** Click `Shortcut`, then `Record` (`hotkey-record-button`). Require `Press the shortcut you want to use.` Press `super+alt+shift+F9` while the `Hotkey` field is focused. Capture the displayed key caps and persisted `hotkey`. If it conflicts, report the error instead of silently choosing another. Click `Hold`, capture `activation_mode: hold`; click `Toggle`, capture `toggle`. Use `Change` then `Cancel` to prove capture cancellation. Clear the shortcut with the visible clear control (`clear-hotkey-button`) and require `Not set` plus persisted `hotkey: null`.
- **General.** Click `General` (`rail-general`). Observe `Appearance` follows the OS. Leave `Launch at login` off for disposable runs. To verify updates when specifically in scope, click `Check now` (`app-update-check-button`) and capture loading and one real terminal result: up to date, available version, or error. `Open release page` is a separate browser entry point; verify its real destination when testing it.

## Gotchas

- Warning badges can append `1 to fix` to a section's accessible name. Match the visible section and fresh tree, not an exact stale name.
- The initial Settings window is scaffolding and does not prove the tray route.
- Voice activation is currently disabled with `Voice activation, coming soon`. Do not force it on because older README text describes an experimental implementation.
- Update checks can reach GitHub; permission actions open System Settings. Neither is part of the safe first proof.
- Login launch needs a dedicated test with OS login-item evidence and teardown. The helper does not remove that system item.
