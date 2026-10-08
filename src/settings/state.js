    // One owner of app state and defaults. `DEFAULT_SETTINGS` is the one home of
    // every fallback setting value, and `setSettings` is the one place
    // `currentSettings` is assigned: every answer from the backend, and every
    // optimistic or reverted guess, flows through here so a missing key lands on
    // its default instead of a value re-spelled inline in each renderer.
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

    // Merge a partial settings answer over the defaults. A partial answer — a
    // settings file from before a setting existed — gets the default for every
    // key it omits, so no renderer has to re-apply one inline.
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
    // The window opens on the first pane with a problem, once, unless the user
    // has already picked a pane themselves.
    let paneChosen = false;
    let searchQuery = "";
    let latestReport = fallbackReport;
    let currentSettings = applySettingsDefaults();
    // The last model status the pane rendered, kept like latestEngines/latestUsage
    // so a download's progress message can name the model it is actually fetching
    // rather than a hardcoded id.
    let latestModelStatus = fallbackModelStatus;
    let dictationBarDisplays = [{ value: "primary", label: "Main display" }];
    let latestEngines = fallbackEngines;
    // Saving the primary engine or the Second Opinion mode share one command, so
    // one flag disables both controls while either is in flight.
    let savingEngineSelection = false;
    // The engine id currently installing or being removed, or null. Only one
    // install/remove runs at a time, same discipline as `downloading` above.
    let installingEngine = null;
    let installProgress = null;
    let latestUsage = fallbackUsage;
    let savingUsage = false;
    // Which destructive Usage action is waiting to be confirmed, or null.
    let pendingUsageConfirm = null;

    // Both of these throw something away that cannot be got back, so both ask
    // first — in the pane, because this webview has no JS confirm dialog.
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

