# Dictation Bar appearance

Users choose the display, edge position, and accent colour of the floating dictation bar and see a preview in Settings.

## Sub-features

- `bar-position`: Left, Centre, Right.
- `bar-colour`: Red, Amber, Green, Blue, Violet, Graphite.
- `bar-display`: selected display or automatic/default display.
- `bar-live`: applied appearance and placement while dictating.

## How to get to it (user POV)

- Open Settings and choose `Dictation Bar`.
- Search `position`, `accent`, or `display` in `Search settings`.
- Start a real dictation to see the live floating bar.

## Driving it with native CUA

Preconditions: doctor passes. Settings changes need no audio permission. Live bar needs the ready dictation setup in [dictation.md](dictation.md).

- **Position.** Click `Dictation Bar` (`rail-bar`). Capture the current preview. Click `Left` in `Dictation bar position` (`position-bottom-left`); require Saved, moved preview, and captured `bar_position: bottom-left`. Repeat `Right`, then restore `Centre` with `bottom-center`.
- **Colour.** Click `Blue` (`accent-blue`), capture the blue preview plus Saved and persisted `accent_color: blue`. Click `Red` to restore `red`. The other choices are accessible buttons named Amber, Green, Violet, and Graphite.
- **Display.** Inspect the native `Dictation Bar display` popup (`bar-display`), click it, then choose an observed display option. Capture the selected display plus real `bar_display`. Do not invent an option for a display that is not attached.
- **Search entry.** Enter `accent` in search, select Blue in results, require the same save/file proof, then restore Red and clear search.
- **Live result.** Start dictation via its configured shortcut. Capture bar placement and colour. While it is visible, change the setting through the real Settings UI and capture the actual bar again. Cancel the disposable speech afterward. The preview alone cannot prove monitor placement or always-on-top behavior.

## Gotchas

- Native AX may not expose a swatch's pressed state. Require the screenshot, Saved message, and real file together.
- Display options depend on current hardware. Record actual display names/values.
- Do not call the bar's backend positioning command directly to fake a live proof.
