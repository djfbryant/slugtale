    const fallbackReport = {
      dictation_available: false,
      items: [
        { id: "microphone", label: "Microphone permission", ready: false, required: true, pane: "privacy" },
        { id: "text_insertion", label: "Text insertion permission", ready: false, required: true, pane: "privacy" },
        { id: "hotkey", label: "Hotkey", ready: false, required: true, pane: "shortcut" },
        { id: "local_model", label: "Local model", ready: false, required: true, pane: "transcription" },
        { id: "transcription_engine", label: "Transcription engine", ready: false, required: true, pane: "transcription" },
        { id: "launch_at_login", label: "Launch at login", ready: true, required: false, pane: "general" }
      ]
    };

    const fallbackModelStatus = {
      id: "base.en",
      filename: "ggml-base.en.bin",
      path: "Model path is available in the desktop app.",
      present: false,
      bytes: null
    };

    const fallbackSettings = {
      hotkey: null,
      activation_mode: "toggle",
      launch_at_login: false,
      diagnostic_logging: false,
      model: null,
      speed_profile: "balanced",
      segment_pause_secs: 5,
      bar_position: "bottom-center",
      accent_color: "red",
      bar_display: "primary",
      primary_engine: "whisper",
      second_opinion: "off",
      transcript_cleanup: "basic",
      voice_activation_enabled: false,
      prefer_built_in_microphone: true
    };

    // Shown only outside the desktop app (no Tauri bridge), so the pane is never
    // blank. Mirrors what a fresh install's Whisper-only Settings File reports.
    const fallbackEngines = [
      {
        id: "whisper",
        display_name: "Whisper base.en",
        is_primary: true,
        metadata: {
          engine: "whisper",
          model_id: "base.en",
          capability: "General-purpose English dictation on a modest model that runs on every platform Slugtale supports. A dependable default for quick notes, short messages, and everyday commands.",
          revision: "ggerganov/whisper.cpp@main",
          approximate_bytes: 155189248,
          source_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
          license: "MIT",
          license_url: "https://github.com/openai/whisper/blob/main/LICENSE",
          attribution: null,
          modifications: "Converted to the GGML format by the whisper.cpp project.",
          system_managed: false,
          supported_platforms: "macOS, Windows, and Linux"
        },
        unavailable_reason: null,
        installable: false,
        assets: { installed_bytes: null, present: false }
      }
    ];

    const BAR_POSITIONS = ["bottom-center", "bottom-left", "bottom-right"];
    const ACCENT_COLORS = ["red", "amber", "green", "blue", "violet", "graphite"];
    const SECOND_OPINION_MODES = ["off", "automatic"];

