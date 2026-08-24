# Settings layout prototype — notes

Throwaway. Delete this file and `src/settings-prototype.html` once a layout wins.
Tracked by bd `slugtale-dsi`.

Run: `npm run prototype:settings`. `←`/`→` switch variant, `s` flips first-run vs
configured, `d` cycles theme — or `?variant=B0|B1|B2|B3&state=fresh|ready&theme=auto|light|dark`.

## Round 1 — which shape? (settled)

Three shapes — merged accordion, two-pane, hero+dense-list — drawn inside the real
480×520 non-resizable frame. **Two-pane won.** What round 1 turned up on the way:

- Today's page **doesn't fit its own window on first run**: everything below Setup,
  including the speed control, is off-screen and the window can't be resized.
- **Readiness and Setup are the same list.** "Hotkey — action needed" and the field
  that sets the hotkey are one idea wearing two hats.
- **Launch at login shows a green check while switched off** — optional items are
  marked ready by definition, so the check reads as a lie.
- **Start Dictation is the biggest control on the page** for a hotkey-driven app, and
  it does nothing (`slugtale-bke`).

## Round 2 — what does a modern two-pane look like? (open)

`B0` is round 1's two-pane, kept for comparison. The other three are all two-pane;
they disagree about how much room the sidebar earns and where status lives.

**B1 — Inset grouped.** Icons in the sidebar; content pane drops per-control cards for
grouped rows with the control on the right — the shape Ventura-and-later System Settings
uses, so it reads as native rather than as a web page. Status stays a pane, but on first
run it lists blockers once as fixable rows and everything else as "Already set". Fits
480×520 exactly in both states.

**B2 — Icon rail.** The rail costs 62px instead of 148px, so content gets 1.5rem headings
and real breathing room; segmented controls replace dropdowns for Activation and Speed.
Status stops being somewhere you go: the top blocker rides at the top of whichever pane
you're on. Most modern-feeling of the three. Trade-off: it surfaces one blocker at a time,
so the rail badges are doing real work.

**B3 — Dense + status bar.** Grouped sidebar with ⌘1–⌘4 hints, dense label/control rows,
and readiness demoted to a permanent bottom status bar (`ready | ⌘⇧D | toggle | base.en ·
balanced`) the way an editor shows branch and errors. Most information per pixel, and the
status bar means you never navigate to check. Trade-off: **the row buttons only appear on
hover**, which is a discoverability and keyboard-access problem — it would need focus-visible
and probably always-on buttons, at which point it looks closer to B1.

### Dark mode

Every colour in the prototype goes through a token in `:root` / `[data-theme="dark"]`,
and `theme=auto` follows `prefers-color-scheme` live. **That token block is the part worth
keeping regardless of which layout wins** — `src/index.html` currently hardcodes ~25 hex
values inline, which is why it has no dark mode today.

Two things the dark pass forced:

- Amber "action needed" surfaces can't just be `#fdf6ec` dimmed; dark needs its own
  warm-dark tint (`--warn-soft: #2a2115`) or the row glows.
- Keycaps need a real border in dark, not just a lighter fill, or they read as flat text.

The real app should default to Automatic and store an explicit override — hence the
Theme row in B1/B2/B3's General pane.

## Recommendation

**B2 for the shape, B1 for the row treatment.** B2's icon rail is the right trade at this
window size and its segmented controls suit three-option settings far better than dropdowns;
B1's grouped rows handle the settings that don't fit a segment (model, paths, toggles) and
degrade better as settings are added. B3's status bar is worth stealing into either.

## Verdict

_(fill in: which variant won, and why)_

## Before deleting

- `src/` is `frontendDist`, so this prototype ships inside a `tauri build` bundle. It is
  inert (nothing loads it) but should not survive to a release.
- Remove the `prototype:settings` script from `package.json` with it.
- Lift the token block into `src/index.html` first — that is the reusable part.
