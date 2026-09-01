# Settings Layout Prototype — Notes

Throwaway. Delete this file and `prototypes/settings/settings-prototype.html` once a layout decision is captured.
Tracked by bd `slugtale-dsi`.

Run: `npm run prototype:settings` or open `prototypes/settings/settings-prototype.html` in your browser.
Interactive controls:
- `o`: Toggle Operating System (**Omarchy Linux / Hyprland** vs. **macOS**)
- `d`: Cycle Theme Palettes (**Gruvbox Dark**, **Gruvbox Light**, **Catppuccin Mocha**, **Tokyo Night**, Standard Dark, Standard Light)
- `s`: Cycle Scenario State (`ready` configured vs. `fresh` unconfigured)
- `1`..`6`: Jump across tabs (Status, Dictation, Bar, Usage, Engine, General)
- Direct URL params: `?os=omarchy|macos&theme=gruvbox-dark|gruvbox-light|catppuccin-mocha|tokyo-night|dark|light&state=ready|fresh`

---

## B1 in Omarchy Linux (Hyprland / Arch)

The winning **B1 (Inset Grouped Rail)** layout adapts to the Omarchy Linux environment:

### 1. Window Frame & Compositor Styling
- **Hyprland Active Border**: Uses a 2px active border glow (`#fe8019` in Gruvbox, `#cba6f7` in Catppuccin) with rounded 10px corners instead of macOS shadow drops.
- **Titlebar / CSD**: Replaces macOS traffic light dots with a minimalist Hyprland header featuring a `Hyprland · Wayland` status pill and right-aligned window action glyphs (`─`, `□`, `✕`).
- **Typography**: Clean monospace / Inter keycaps and crisp high-contrast border lines matching Omarchy standards.

### 2. Linux Keyboard Shortcuts & Keycaps
- **Modifiers**: Uses `<kbd>Super</kbd> + <kbd>Shift</kbd> + <kbd>D</kbd>` (or `<kbd>Ctrl</kbd> + <kbd>Alt</kbd>`) instead of macOS `⌘` (Cmd) and `⌥` (Option) glyphs.
- **Global Keybinding Seam**: Reflects global shortcuts bound through the Linux Platform Adapter (Hyprland / wlroots).

### 3. Native Linux Domain & Platform Readiness
- **Display Server Session**: Reports `Wayland (Hyprland compositor connected)` with readiness check.
- **Audio Capture**: Connects to `PipeWire Audio Capture (ALSA / PulseAudio)`.
- **Text Insertion**: Uses `wlr-virtual-keyboard` / Wayland Accessibility portals.
- **Local Storage**: Paths default to `~/.local/share/slugtale/` and `~/.config/slugtale/`.

### 4. Color Palettes (Omarchy Theming Integration)
- **Gruvbox Dark**: `#282828` background, warm amber/orange `#fe8019` accent, `#ebdbb2` text, `#b8bb26` green ok indicator.
- **Gruvbox Light**: `#fbf1c7` background, `#d65d0e` rust accent, `#282828` text.
- **Catppuccin Mocha**: `#1e1e2e` base, `#cba6f7` mauve / `#89b4fa` blue accents, `#cdd6f4` text.
- **Tokyo Night**: `#1a1b26` background, `#7aa2f7` blue accent, `#c0caf5` text.
