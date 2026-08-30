# macOS First with Platform Adapters

Slugtale's first implementation slice will target macOS first while shaping platform-specific behavior behind adapters for later Windows and Linux ports. This keeps the first vertical slice small enough to validate end to end, without baking macOS-only assumptions into the core dictation workflow.

## Decision: spawned OS processes are adapter-owned

The adapter boundary owns every spawned OS process. This covers sound cues, notifications, settings deep links, and file-manager reveal (`open`, `explorer`, `xdg-open`, `afplay`, `osascript`).

Domain modules decide *what* to do and dispatch through `#[cfg]` arms to adapter functions. Adapters own *how* the OS does it, including capability differences. Example: `xdg-open` cannot select a file, so the Linux adapter opens the parent folder instead. Platforms without an adapter get a no-op or an error at the dispatch site, never a direct OS call in a domain module.
