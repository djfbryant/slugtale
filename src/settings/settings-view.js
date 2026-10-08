    function renderSettings(settings, message = "", isError = false) {
      currentSettings = { ...fallbackSettings, ...settings };
      const hotkeyInput = document.getElementById("hotkey-input");
      const hotkeyDisplay = document.getElementById("hotkey-display");
      const recordButton = document.getElementById("hotkey-record-button");
      const clearButton = document.getElementById("clear-hotkey-button");
      const messageEl = document.getElementById("settings-message");

      hotkeyInput.value = currentSettings.hotkey || "";
      hotkeyInput.hidden = !capturingHotkey;
      hotkeyInput.disabled = savingSettings;
      hotkeyDisplay.hidden = capturingHotkey;
      hotkeyDisplay.innerHTML = currentSettings.hotkey
        ? keycapsHtml(currentSettings.hotkey)
        : `<span class="row-sub">Not set</span>`;
      recordButton.textContent = capturingHotkey
        ? "Cancel"
        : currentSettings.hotkey ? "Change" : "Record";
      recordButton.disabled = savingSettings;
      clearButton.hidden = !currentSettings.hotkey || capturingHotkey;
      clearButton.disabled = savingSettings || !currentSettings.hotkey;
      messageEl.classList.toggle("error", isError);
      messageEl.textContent = message;

      const mode = currentSettings.activation_mode || "toggle";
      document.getElementById("mode-toggle").setAttribute("aria-pressed", String(mode === "toggle"));
      document.getElementById("mode-hold").setAttribute("aria-pressed", String(mode === "hold"));
      document.getElementById("mode-toggle").disabled = savingSettings;
      document.getElementById("mode-hold").disabled = savingSettings;

      const profile = currentSettings.speed_profile || "balanced";
      ["fast", "balanced", "accurate"].forEach((option) => {
        const button = document.getElementById(`speed-${option}`);
        button.setAttribute("aria-pressed", String(option === profile));
        button.disabled = savingProfile;
      });

      SEGMENT_PAUSE_OPTIONS.forEach((option) => {
        const button = document.getElementById(`pause-${option}`);
        button.setAttribute("aria-pressed", String(option === currentSettings.segment_pause_secs));
        button.disabled = savingPause;
      });

      const cleanupMode = currentSettings.transcript_cleanup || "basic";
      TRANSCRIPT_CLEANUP_MODES.forEach((option) => {
        const button = document.getElementById(`cleanup-${option}`);
        button.setAttribute("aria-pressed", String(option === cleanupMode));
        button.disabled = savingCleanup;
      });
      const example = CLEANUP_EXAMPLES[cleanupMode] || CLEANUP_EXAMPLES.basic;
      document.getElementById("cleanup-example-said").innerHTML = example.said;
      document.getElementById("cleanup-example-typed").textContent = example.typed;

      const barPosition = currentSettings.bar_position || "bottom-center";
      BAR_POSITIONS.forEach((option) => {
        const button = document.getElementById(`position-${option}`);
        button.setAttribute("aria-pressed", String(option === barPosition));
        button.disabled = savingDictationBar;
      });

      const accent = currentSettings.accent_color || "red";
      ACCENT_COLORS.forEach((option) => {
        const button = document.getElementById(`accent-${option}`);
        button.setAttribute("aria-pressed", String(option === accent));
        button.disabled = savingDictationBar;
      });
      const preview = document.getElementById("bar-preview-pill");
      preview.dataset.position = barPosition;
      preview.dataset.accent = accent;

      renderDictationBarDisplays();

      const secondOpinion = currentSettings.second_opinion || "off";
      SECOND_OPINION_MODES.forEach((option) => {
        const button = document.getElementById(`second-opinion-${option}`);
        button.setAttribute("aria-pressed", String(option === secondOpinion));
        button.disabled = savingEngineSelection;
      });

      const launchToggle = document.getElementById("launch-at-login-toggle");
      launchToggle.checked = Boolean(currentSettings.launch_at_login);
      launchToggle.disabled = savingLaunchAtLogin;

      const microphoneToggle = document.getElementById("prefer-built-in-microphone-toggle");
      microphoneToggle.checked = currentSettings.prefer_built_in_microphone !== false;
      microphoneToggle.disabled = savingMicrophone;

      const voiceToggle = document.getElementById("voice-activation-toggle");
      const voiceMessage = document.getElementById("voice-activation-message");
      voiceToggle.checked = false;
      voiceToggle.disabled = true;
      voiceMessage.textContent = "";
    }

    function barDisplayKey(display) {
      return display === "primary" ? "primary" : `monitor:${display.monitor}`;
    }

    function displayFromBarDisplayKey(key) {
      return key === "primary" ? "primary" : { monitor: key.slice("monitor:".length) };
    }

    function renderDictationBarDisplays() {
      const select = document.getElementById("bar-display");
      const selected = currentSettings.bar_display || "primary";
      const selectedKey = barDisplayKey(selected);
      const displays = dictationBarDisplays.some((display) => display.value === "primary")
        ? dictationBarDisplays
        : [{ value: "primary", label: "Main display" }, ...dictationBarDisplays];

      select.replaceChildren(...displays.map((display) => {
        const option = document.createElement("option");
        option.value = barDisplayKey(display.value);
        option.textContent = display.label;
        return option;
      }));
      select.value = displays.some((display) => barDisplayKey(display.value) === selectedKey)
        ? selectedKey
        : "primary";
      select.disabled = savingDictationBar;
    }

