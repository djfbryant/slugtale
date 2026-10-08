    async function loadSettings() {
      const invoke = tauriInvoke();
      if (!invoke) {
        renderSettings(setSettings());
        return;
      }

      try {
        const settings = await invoke("get_settings");
        renderSettings(setSettings(settings));
      } catch (error) {
        console.error("Could not load settings", error);
        renderSettings(setSettings(), String(error), true);
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
        activation_mode: activationMode || currentSettings.activation_mode
      };

      capturingHotkey = false;

      if (!invoke) {
        renderSettings(setSettings(nextSettings));
        return;
      }

      savingSettings = true;
      renderSettings(setSettings(nextSettings), "Saving…");

      try {
        const saved = await invoke("save_hotkey_settings", {
          hotkey: nextSettings.hotkey,
          activationMode: nextSettings.activation_mode
        });
        savingSettings = false;
        renderSettings(setSettings(saved), "Saved.");
        await loadReadiness();
      } catch (error) {
        savingSettings = false;
        renderSettings(setSettings(previousSettings), String(error), true);
      }
    }

