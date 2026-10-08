    // View renderers paint the `settings` they are handed. They never assign
    // `currentSettings` and never re-apply a default inline: the caller passes a
    // merged value (see `setSettings` in state.js), so every value here is already
    // on a real setting and each control reads it directly.
    function renderSettings(settings, message = "", isError = false) {
      const messageEl = document.getElementById("settings-message");
      messageEl.classList.toggle("error", isError);
      messageEl.textContent = message;

      renderHotkeyShortcut(settings);
      renderActivationMode(settings);
      renderSpeedProfile(settings);
      renderSegmentPause(settings);
      renderTranscriptCleanup(settings);
      renderBarAppearance(settings);
      renderSecondOpinion(settings);
      renderLaunchToggle(settings);
      renderMicrophoneToggle(settings);
      renderVoiceActivation();
    }

    function renderHotkeyShortcut(settings) {
      const hotkeyInput = document.getElementById("hotkey-input");
      const hotkeyDisplay = document.getElementById("hotkey-display");
      const recordButton = document.getElementById("hotkey-record-button");
      const clearButton = document.getElementById("clear-hotkey-button");

      hotkeyInput.value = settings.hotkey || "";
      hotkeyInput.hidden = !capturingHotkey;
      hotkeyInput.disabled = savingSettings;
      hotkeyDisplay.hidden = capturingHotkey;
      hotkeyDisplay.innerHTML = settings.hotkey
        ? keycapsHtml(settings.hotkey)
        : `<span class="row-sub">Not set</span>`;
      recordButton.textContent = capturingHotkey
        ? "Cancel"
        : settings.hotkey ? "Change" : "Record";
      recordButton.disabled = savingSettings;
      clearButton.hidden = !settings.hotkey || capturingHotkey;
      clearButton.disabled = savingSettings || !settings.hotkey;
    }

    function renderActivationMode(settings) {
      const mode = settings.activation_mode;
      document.getElementById("mode-toggle").setAttribute("aria-pressed", String(mode === "toggle"));
      document.getElementById("mode-hold").setAttribute("aria-pressed", String(mode === "hold"));
      document.getElementById("mode-toggle").disabled = savingSettings;
      document.getElementById("mode-hold").disabled = savingSettings;
    }

    function renderSpeedProfile(settings) {
      const profile = settings.speed_profile;
      ["fast", "balanced", "accurate"].forEach((option) => {
        const button = document.getElementById(`speed-${option}`);
        button.setAttribute("aria-pressed", String(option === profile));
        button.disabled = savingProfile;
      });
    }

    function renderSegmentPause(settings) {
      SEGMENT_PAUSE_OPTIONS.forEach((option) => {
        const button = document.getElementById(`pause-${option}`);
        button.setAttribute("aria-pressed", String(option === settings.segment_pause_secs));
        button.disabled = savingPause;
      });
    }

    function renderTranscriptCleanup(settings) {
      const cleanupMode = settings.transcript_cleanup;
      TRANSCRIPT_CLEANUP_MODES.forEach((option) => {
        const button = document.getElementById(`cleanup-${option}`);
        button.setAttribute("aria-pressed", String(option === cleanupMode));
        button.disabled = savingCleanup;
      });
      const example = CLEANUP_EXAMPLES[cleanupMode] || CLEANUP_EXAMPLES.basic;
      document.getElementById("cleanup-example-said").innerHTML = example.said;
      document.getElementById("cleanup-example-typed").textContent = example.typed;
    }

    function renderBarAppearance(settings) {
      const barPosition = settings.bar_position;
      BAR_POSITIONS.forEach((option) => {
        const button = document.getElementById(`position-${option}`);
        button.setAttribute("aria-pressed", String(option === barPosition));
        button.disabled = savingDictationBar;
      });

      const accent = settings.accent_color;
      ACCENT_COLORS.forEach((option) => {
        const button = document.getElementById(`accent-${option}`);
        button.setAttribute("aria-pressed", String(option === accent));
        button.disabled = savingDictationBar;
      });
      const preview = document.getElementById("bar-preview-pill");
      preview.dataset.position = barPosition;
      preview.dataset.accent = accent;

      renderDictationBarDisplays(settings);
    }

    function renderSecondOpinion(settings) {
      const secondOpinion = settings.second_opinion;
      SECOND_OPINION_MODES.forEach((option) => {
        const button = document.getElementById(`second-opinion-${option}`);
        button.setAttribute("aria-pressed", String(option === secondOpinion));
        button.disabled = savingEngineSelection;
      });
    }

    function renderLaunchToggle(settings) {
      const launchToggle = document.getElementById("launch-at-login-toggle");
      launchToggle.checked = Boolean(settings.launch_at_login);
      launchToggle.disabled = savingLaunchAtLogin;
    }

    function renderMicrophoneToggle(settings) {
      const microphoneToggle = document.getElementById("prefer-built-in-microphone-toggle");
      microphoneToggle.checked = settings.prefer_built_in_microphone !== false;
      microphoneToggle.disabled = savingMicrophone;
    }

    // Voice Activation is not wired yet (slugtale-e95): the row is always disabled
    // and cleared, whatever the settings file holds.
    function renderVoiceActivation() {
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

    function renderDictationBarDisplays(settings) {
      const select = document.getElementById("bar-display");
      const selected = settings.bar_display;
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
