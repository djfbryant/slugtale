    // Permission guidance is worded per platform: macOS and Windows gate
    // microphone and (on macOS) text insertion behind OS permission panels,
    // while Linux has no per-process gate — the checks confirm a mic is present
    // and the session is X11 (ADR-0021, ADR-0023).
    const PLATFORM = (() => {
      const ua = (typeof navigator !== "undefined" ? navigator.userAgent || "" : "").toLowerCase();
      if (ua.includes("windows")) return "windows";
      if (ua.includes("mac os") || ua.includes("macintosh")) return "macos";
      if (ua.includes("linux") || ua.includes("x11")) return "linux";
      return "macos";
    })();

    // Everything the settings window knows about each Dictation Readiness item
    // that is not a fact about this machine: what to tell the user, how to send
    // them to the right system-settings panel on their platform, and nothing
    // else. One record per item, so an id cannot appear without its copy.
    //
    // The pane is not here: the report carries it, because which pane settles an
    // item is a fact about Slugtale's own settings window rather than about the
    // platform.
    const READINESS_COPY = {
      microphone: {
        guidance: {
          macos: "Open macOS Privacy & Security and allow Slugtale to capture audio.",
          windows: "Open Windows microphone privacy settings and let desktop apps access your microphone.",
          linux: "Connect a microphone. Linux has no per-app permission — Slugtale just needs an input device."
        },
        actions: {
          macos: { command: "open_microphone_settings", label: "Open Privacy" },
          windows: { command: "open_microphone_settings", label: "Open Settings" },
          linux: { command: "open_microphone_settings", label: "Open Sound Settings" }
        }
      },
      text_insertion: {
        guidance: {
          macos: "Open macOS Accessibility and allow Slugtale to write into the current text target.",
          windows: "No setup needed — Windows lets Slugtale type into the focused app.",
          linux: "Use an X11 session. Slugtale types into other apps on X11; Wayland support is coming."
        },
        actions: {
          macos: { command: "open_text_insertion_settings", label: "Open Accessibility" }
        }
      },
      hotkey: {
        guidance: "Choose the hotkey that starts dictation while another app has focus."
      },
      local_model: {
        guidance: "Download the local model before transcription can run."
      },
      // Fallback only: the backend sends the engine's own reason as `detail`,
      // because "this build has no Whisper" and "the model is not installed"
      // need different actions from the user.
      transcription_engine: {
        guidance: "No transcription engine can run in this build."
      },
      launch_at_login: {
        guidance: "Optional. Start Slugtale automatically when you sign in."
      }
    };

