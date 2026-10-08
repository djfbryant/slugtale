    async function loadSettings() {
      const invoke = tauriInvoke();
      if (!invoke) {
        renderSettings(fallbackSettings);
        return;
      }

      try {
        const settings = await invoke("get_settings");
        renderSettings(settings);
      } catch (error) {
        console.error("Could not load settings", error);
        renderSettings(fallbackSettings, String(error), true);
      }
    }

    async function loadDictationBarDisplays() {
      const invoke = tauriInvoke();
      if (!invoke) return;

      try {
        dictationBarDisplays = await invoke("dictation_bar_displays");
        renderSettings(currentSettings);
      } catch (error) {
        console.error("Could not load Dictation Bar displays", error);
      }
    }

    async function saveHotkeySettings(hotkey, activationMode) {
      const invoke = tauriInvoke();
      const previousSettings = { ...currentSettings };
      const nextSettings = {
        ...currentSettings,
        hotkey: hotkey || null,
        activation_mode: activationMode || currentSettings.activation_mode || "toggle"
      };

      capturingHotkey = false;

      if (!invoke) {
        renderSettings(nextSettings);
        return;
      }

      savingSettings = true;
      renderSettings(nextSettings, "Saving…");

      try {
        const saved = await invoke("save_hotkey_settings", {
          hotkey: nextSettings.hotkey,
          activationMode: nextSettings.activation_mode
        });
        savingSettings = false;
        renderSettings(saved, "Saved.");
        await loadReadiness();
      } catch (error) {
        savingSettings = false;
        renderSettings(previousSettings, String(error), true);
      }
    }

