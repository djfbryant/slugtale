    let downloading = false;
    let savingSettings = false;
    let savingProfile = false;
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
    let currentSettings = { ...fallbackSettings };
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

