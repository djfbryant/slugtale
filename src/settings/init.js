    function init() {
      PANES.forEach((pane) => {
        const button = document.getElementById(`rail-${pane.id}`);
        document.getElementById(`rail-icon-${pane.id}`).innerHTML = iconSvg(pane.icon, 13);
        button.addEventListener("click", () => {
          const search = document.getElementById("settings-search");
          search.value = "";
          searchQuery = "";
          selectPane(pane.id, { byUser: true });
        });
      });
      document.querySelectorAll(".pane-heading-icon").forEach((icon) => {
        const pane = PANES.find((entry) => entry.id === icon.dataset.group);
        if (pane) icon.innerHTML = iconSvg(pane.icon, 11);
      });

      document.getElementById("search-icon").innerHTML = iconSvg("search", 14);
      const search = document.getElementById("settings-search");
      search.addEventListener("input", () => setSearchQuery(search.value));
      search.addEventListener("keydown", (event) => {
        if (event.key !== "Escape") return;
        search.value = "";
        setSearchQuery("");
      });

      // There is no in-app theme choice: the window follows the system, and the
      // General pane says which way it currently is.
      renderAppearance();
      window.matchMedia?.("(prefers-color-scheme: dark)").addEventListener?.("change", renderAppearance);

      document.getElementById("clear-hotkey-button").textContent = "Clear";
      setButton(document.getElementById("download-model-button"), "download", "Download");
      setButton(document.getElementById("reveal-model-button"), "folder", "Reveal");
      setButton(document.getElementById("delete-model-button"), "trash", "Delete");

      document.getElementById("download-model-button").addEventListener("click", downloadModel);
      document.getElementById("reveal-model-button").addEventListener("click", revealModel);
      document.getElementById("delete-model-button").addEventListener("click", deleteModel);

      document.getElementById("hotkey-record-button").addEventListener("click", () => {
        if (capturingHotkey) {
          stopHotkeyCapture();
          return;
        }
        startHotkeyCapture();
      });
      document.getElementById("clear-hotkey-button").addEventListener("click", () => {
        saveHotkeySettings(null);
      });
      document.getElementById("hotkey-input").addEventListener("keydown", (event) => {
        event.preventDefault();
        if (event.key === "Escape") {
          stopHotkeyCapture();
          return;
        }
        const captured = hotkeyFromKeyboardEvent(event);
        if (!captured) return;
        if (captured.error) {
          renderSettings(currentSettings, captured.error, true);
          return;
        }
        saveHotkeySettings(captured.hotkey);
      });
      document.getElementById("hotkey-input").addEventListener("blur", () => {
        if (capturingHotkey) stopHotkeyCapture();
      });

      ["toggle", "hold"].forEach((mode) => {
        document.getElementById(`mode-${mode}`).addEventListener("click", () => {
          if (mode === currentSettings.activation_mode) return;
          saveHotkeySettings(currentSettings.hotkey, mode);
        });
      });

      ["fast", "balanced", "accurate"].forEach((profile) => {
        document.getElementById(`speed-${profile}`).addEventListener("click", () => {
          if (profile === currentSettings.speed_profile) return;
          saveTranscriptionSettings(profile);
        });
      });

      SEGMENT_PAUSE_OPTIONS.forEach((secs) => {
        document.getElementById(`pause-${secs}`).addEventListener("click", () => {
          if (secs === currentSettings.segment_pause_secs) return;
          saveSegmentPauseSettings(secs);
        });
      });

      TRANSCRIPT_CLEANUP_MODES.forEach((mode) => {
        document.getElementById(`cleanup-${mode}`).addEventListener("click", () => {
          if (mode === (currentSettings.transcript_cleanup || "basic")) return;
          saveTranscriptCleanupSettings(mode);
        });
      });

      BAR_POSITIONS.forEach((position) => {
        document.getElementById(`position-${position}`).addEventListener("click", () => {
          if (position === currentSettings.bar_position) return;
          saveDictationBarSettings({ barPosition: position });
        });
      });

      ACCENT_COLORS.forEach((accent) => {
        document.getElementById(`accent-${accent}`).addEventListener("click", () => {
          if (accent === currentSettings.accent_color) return;
          saveDictationBarSettings({ accentColor: accent });
        });
      });

      document.getElementById("bar-display").addEventListener("change", (event) => {
        const display = displayFromBarDisplayKey(event.target.value);
        if (barDisplayKey(display) === barDisplayKey(currentSettings.bar_display || "primary")) {
          return;
        }
        saveDictationBarSettings({ barDisplay: display });
      });

      SECOND_OPINION_MODES.forEach((mode) => {
        document.getElementById(`second-opinion-${mode}`).addEventListener("click", () => {
          if (mode === currentSettings.second_opinion) return;
          saveEngineSettings({ secondOpinion: mode });
        });
      });

      document.getElementById("launch-at-login-toggle").addEventListener("change", (event) => {
        saveLaunchAtLogin(event.target.checked);
      });

      document.getElementById("voice-activation-toggle").addEventListener("change", (event) => {
        saveVoiceActivation(event.target.checked);
      });
      document.getElementById("prefer-built-in-microphone-toggle").addEventListener("change", (event) => {
        saveMicrophonePreference(event.target.checked);
      });

      document.getElementById("app-update-check-button").addEventListener("click", () => {
        checkForAppUpdate();
      });
      document.getElementById("app-update-release-button").addEventListener("click", openAppUpdateRelease);

      document.getElementById("usage-store-toggle").addEventListener("change", (event) => {
        setUsageStoring(event.target.checked);
      });
      document.getElementById("usage-baseline-button").addEventListener("click", openTypingChallenge);
      document.getElementById("usage-redo-button").addEventListener("click", askRedoTypingChallenges);
      document.getElementById("usage-confirm-cancel").addEventListener("click", cancelUsageConfirm);
      document.getElementById("usage-confirm-accept").addEventListener("click", acceptUsageConfirm);
      document.getElementById("usage-estimate-save").addEventListener("click", saveTypingEstimate);
      document.getElementById("usage-estimate-clear").addEventListener("click", () => {
        document.getElementById("usage-estimate-input").value = "";
        saveTypingEstimate();
      });
      document.getElementById("usage-estimate-input").addEventListener("keydown", (event) => {
        if (event.key === "Enter") saveTypingEstimate();
      });

      // The only push Usage gets: a Counted Segment landed, or the Typing
      // Challenge window changed the baseline every number here is read against.
      const listen = tauriEvent();
      if (listen) listen("usage-changed", () => loadUsage());

      window.addEventListener("focus", () => {
        loadReadiness({ background: true });
        loadUsage();
      });
      document.addEventListener("visibilitychange", () => {
        if (document.visibilityState === "visible") {
          loadReadiness({ background: true });
          loadUsage();
        }
      });

      selectPane("shortcut");
      refreshSetup();
    }

    init();
