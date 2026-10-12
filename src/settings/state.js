    const DEFAULT_SETTINGS = {
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

    function applySettingsDefaults(settings) {
      return { ...DEFAULT_SETTINGS, ...(settings || {}) };
    }

    function setSettings(settings) {
      currentSettings = applySettingsDefaults(settings);
      return currentSettings;
    }

    let downloading = false;
    let savingSettings = false;
    let savingProfile = false;
    let savingPause = false;
    let savingCleanup = false;
    let savingDictationBar = false;
    let savingLaunchAtLogin = false;
    let savingMicrophone = false;
    let savingVoiceActivation = false;
    let openingReadinessAction = false;
    let capturingHotkey = false;
    let activePane = "shortcut";
    let paneChosen = false;
    let searchQuery = "";
    let latestReport = fallbackReport;
    let currentSettings = applySettingsDefaults();
    let latestModelStatus = fallbackModelStatus;
    let dictationBarDisplays = [{ value: "primary", label: "Main display" }];
    let latestEngines = fallbackEngines;
    let savingEngineSelection = false;
    let installingEngine = null;
    let installProgress = null;
    let latestUsage = fallbackUsage;
    let savingUsage = false;
    let pendingUsageConfirm = null;

    const USAGE_CONFIRMATIONS = {
      "stop-storing": {
        text: "Stop storing usage counts and delete the ones already stored? Your measured typing speed is kept.",
        accept: "Delete counts"
      },
      redo: {
        text: "Take all three typing challenges again? This replaces your measured speed, and the time saved shown here will move with it.",
        accept: "Redo challenges"
      }
    };

