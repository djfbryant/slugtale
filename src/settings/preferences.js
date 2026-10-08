    // These three preferences write one field of the settings file and update
    // their own row. They read and write `currentSettings` through `setSettings`
    // (state.js) like every other writer, rather than re-merging defaults here.
    async function saveLaunchAtLogin(enabled) {
      const invoke = tauriInvoke();
      const previousSettings = { ...currentSettings };
      const toggle = document.getElementById("launch-at-login-toggle");
      const messageEl = document.getElementById("launch-at-login-message");

      if (!invoke) {
        setSettings({ ...currentSettings, launch_at_login: enabled });
        toggle.checked = enabled;
        return;
      }

      savingLaunchAtLogin = true;
      toggle.disabled = true;
      messageEl.classList.remove("error");
      messageEl.textContent = "Saving…";

      try {
        const saved = await invoke("save_launch_at_login", { enabled });
        savingLaunchAtLogin = false;
        setSettings(saved);
        toggle.checked = Boolean(currentSettings.launch_at_login);
        toggle.disabled = false;
        messageEl.textContent = currentSettings.launch_at_login
          ? "Slugtale will launch when you sign in."
          : "";
      } catch (error) {
        savingLaunchAtLogin = false;
        setSettings(previousSettings);
        toggle.checked = Boolean(currentSettings.launch_at_login);
        toggle.disabled = false;
        messageEl.classList.add("error");
        messageEl.textContent = String(error);
      }
    }

    // The next dictation opens the chosen microphone; nothing to restart.
    async function saveMicrophonePreference(enabled) {
      const invoke = tauriInvoke();
      const previousSettings = { ...currentSettings };
      const toggle = document.getElementById("prefer-built-in-microphone-toggle");
      const messageEl = document.getElementById("microphone-message");

      if (!invoke) {
        setSettings({ ...currentSettings, prefer_built_in_microphone: enabled });
        toggle.checked = enabled;
        return;
      }

      savingMicrophone = true;
      toggle.disabled = true;
      messageEl.classList.remove("error");
      messageEl.textContent = "Saving…";

      try {
        const saved = await invoke("save_microphone_settings", { preferBuiltInMicrophone: enabled });
        savingMicrophone = false;
        setSettings(saved);
        toggle.checked = currentSettings.prefer_built_in_microphone !== false;
        toggle.disabled = false;
        messageEl.textContent = "";
      } catch (error) {
        savingMicrophone = false;
        setSettings(previousSettings);
        toggle.checked = currentSettings.prefer_built_in_microphone !== false;
        toggle.disabled = false;
        messageEl.classList.add("error");
        messageEl.textContent = String(error);
      }
    }

    // Voice Activation (slugtale-e95). Unsupported builds hide the row. The
    // backend starts or stops the listener before it saves the checkbox value.
    async function saveVoiceActivation(enabled) {
      const invoke = tauriInvoke();
      const previousSettings = { ...currentSettings };
      const toggle = document.getElementById("voice-activation-toggle");
      const messageEl = document.getElementById("voice-activation-message");

      if (!invoke) {
        setSettings({ ...currentSettings, voice_activation_enabled: enabled });
        toggle.checked = enabled;
        return;
      }

      savingVoiceActivation = true;
      toggle.disabled = true;
      messageEl.classList.remove("error");
      messageEl.textContent = "Saving…";

      try {
        const saved = await invoke("save_voice_activation_settings", { enabled });
        savingVoiceActivation = false;
        setSettings(saved);
        toggle.checked = Boolean(currentSettings.voice_activation_enabled);
        toggle.disabled = false;
        messageEl.textContent = currentSettings.voice_activation_enabled
          ? "Listening for \"Hi Slugtale\". Wait for the dictation bar, then talk."
          : "";
      } catch (error) {
        savingVoiceActivation = false;
        setSettings(previousSettings);
        toggle.checked = Boolean(currentSettings.voice_activation_enabled);
        toggle.disabled = false;
        messageEl.classList.add("error");
        messageEl.textContent = String(error);
      }
    }
