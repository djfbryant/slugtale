# Dictation bar prototype — notes

Throwaway. Delete this file and `src/dictation-bar-prototype.html` once a variant wins.
Tracked by bd `slugtale-gob`.

Run: `npm run prototype:dictation-bar` (`←`/`→` variant, `p` phase, `o` position,
`s` speech sim, `f` window outline — or `?variant=A|B|C1|C2|C3&phase=…&pos=centre|corner`).
**Hover the orb** — C3 only makes sense in motion.

## Question

Round 1 asked what the Dictation Bar should look like. The answer that came back was
"the orb direction", so round 2 narrows it:

> If the bar is a small ambient shape rather than a control panel, **what shape**, and
> what does it do when it has something to say?

## What round 1 established

A (Ambient) and B (Console) are still in the prototype as reference points, but they are
not candidates. They bracket the range: A says nothing, B says everything. The orb sits
outside that axis entirely — it is not a smaller bar, it is a different object.

Round 1's orb had three unresolved weaknesses, and each C-variant is an answer to one:

1. **Transcribing was only a colour change.** → C1 changes silhouette.
2. **Controls were undiscoverable behind a hover.** → C3 morphs to reveal them.
3. **It wasn't clear the orb needed to say anything at all.** → C2 tests the floor.

## Round 2: three orbs

**C1 — Halo (96×100, 65% smaller than today's window).** A radial waveform: 24 rays ringed
around a dark core with the mic glyph, elapsed clock underneath. The level reading *is* the
silhouette, so it stays legible in peripheral vision. Transcribing dims the rays and sweeps
a bright arc around the ring — a genuinely different shape, not a recoloured one.
Weakness: 24 animating rays at 60fps is the most expensive thing here, and the clock pins
the window 40px taller than it would otherwise need.

**C2 — Pearl (60×60, 87% smaller).** The floor. One soft blob, no ring, no bars, no clock,
**no controls at all**. Level drives scale and glow only. Transcribing desaturates it to
grey and breathes slowly. Nothing to read, only something to notice.
Weakness: this is the honest extreme — with no visible affordance, Escape and the hotkey
are the *only* ways out, so it is a bet that the keyboard path always works. If insertion
ever fails silently, the user has nothing to click.

**C3 — Morph (232×60, 49% smaller).** An orb at rest, a bar when you engage. Collapsed it
is a 44px circle; on hover — and automatically while transcribing — it grows sideways into
a pill with the state, the clock, and Stop/Cancel. Pays for the controls only in the
moments you want them.
Weakness: the transparent window has to be sized for the *expanded* state permanently, so
86% of it is invisible-but-clickable while collapsed (see below).

## The finding that only showed up at true size

The readout's `dead` line measures how much of the transparent window is not painted. A
transparent Tauri window still receives mouse events across its whole rect unless cursor
events are explicitly ignored, so **dead space is surface stolen from the app underneath**:

| variant | window | dead (collapsed) |
|---------|--------|------------------|
| C1 Halo  | 96×100  | 52% |
| C2 Pearl | 60×60   | 50% |
| C3 Morph | 232×60  | **86%** → 27% expanded |

C3 parks a 232px-wide invisible click-trap over the user's document for the entire
recording. That is not visible in a mockup and it is not visible in the code — it only
appears when the window bounds are drawn. Whichever variant wins, the shipping bar
probably needs `set_ignore_cursor_events` on the transparent margin, which is a real
follow-up regardless of the visual outcome.

## Also worth trying while flipping

`o` toggles bottom-centre vs bottom-right. The orb is the first version small enough that
the corner is viable, and the corner does not cover the line you are dictating into — the
one thing the notes in the fake editor keep insisting the bar must not do. The bar shape
never had this option.

## Open questions this does not answer

- Is the elapsed clock useful or just anxious? C1 shows it always, C3 only when expanded,
  C2 never.
- Does the orb read as Slugtale, or as a generic system artefact? Nothing here carries any
  brand, and at 44px there may not be room for any.

## Verdict

**C3 — Morph wins.** An orb at rest that grows into a bar on hover, and automatically for
the whole transcribing phase. It is the only variant that does not have to choose between
"says nothing" and "says everything" — it says nothing until you ask, and says everything
when there is something to report.

Two settings came out of the prototype that nobody asked for up front:

- **Position** (bottom-centre / bottom-left / bottom-right). Only viable because the orb is
  small enough that a corner no longer covers the line you are dictating into.
- **Accent colour**, from a fixed six-swatch palette. Once the resting state is a 44px
  circle with no label, the colour *is* the app's identity in use. Fixed palette rather
  than free hex: an arbitrary colour can be illegible on the dark translucent pill, and a
  user-supplied hex would end up interpolated into CSS.

Implementation is tracked by bd `slugtale-z7a`, which also carries the click-trap fix and
deletes this prototype. **Do not promote the prototype CSS directly** — it was written with
no tests and no reduced-motion handling, and `.v-c3` in particular depends on a full-width
ancestor that the real bar does not have yet.
