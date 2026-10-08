    async function saveTranscriptionSettings(speedProfile) {
      const invoke = tauriInvoke();
      const previousSettings = { ...currentSettings };
      const nextSettings = { ...currentSettings, speed_profile: speedProfile };
      const messageEl = document.getElementById("transcription-message");

      if (!invoke) {
        renderSettings(nextSettings);
        return;
      }

      savingProfile = true;
      renderSettings(nextSettings);
      messageEl.classList.remove("error");
      messageEl.textContent = "Saving…";

      try {
        const saved = await invoke("save_transcription_settings", { speedProfile });
        savingProfile = false;
        renderSettings(saved);
        messageEl.textContent = "Saved. Applies to your next dictation.";
      } catch (error) {
        savingProfile = false;
        renderSettings(previousSettings);
        messageEl.classList.add("error");
        messageEl.textContent = String(error);
      }
    }

    async function saveTranscriptCleanupSettings(cleanupMode) {
      const invoke = tauriInvoke();
      const previousSettings = { ...currentSettings };
      const nextSettings = { ...currentSettings, transcript_cleanup: cleanupMode };
      const messageEl = document.getElementById("cleanup-message");

      if (!invoke) {
        renderSettings(nextSettings);
        return;
      }

      savingCleanup = true;
      renderSettings(nextSettings);
      messageEl.classList.remove("error");
      messageEl.textContent = "Saving…";

      try {
        const saved = await invoke("save_transcript_cleanup_settings", { cleanupMode });
        savingCleanup = false;
        renderSettings(saved);
        messageEl.textContent = cleanupMode !== "basic"
          ? "Saved. Applies to your next dictation."
          : "Saved.";
      } catch (error) {
        savingCleanup = false;
        renderSettings(previousSettings);
        messageEl.classList.add("error");
        messageEl.textContent = String(error);
      }
    }

    // Position, accent, and display save together: all describe where and how
    // the Dictation Bar appears, and the backend applies them to a bar already
    // on screen so the user can judge the choice while making it.
    async function saveDictationBarSettings({ barPosition, accentColor, barDisplay }) {
      const invoke = tauriInvoke();
      const previousSettings = { ...currentSettings };
      const nextPosition = barPosition || currentSettings.bar_position || "bottom-center";
      const nextAccent = accentColor || currentSettings.accent_color || "red";
      const nextDisplay = barDisplay || currentSettings.bar_display || "primary";
      const nextSettings = {
        ...currentSettings,
        bar_position: nextPosition,
        accent_color: nextAccent,
        bar_display: nextDisplay
      };
      const messageEl = document.getElementById("dictation-bar-message");

      if (!invoke) {
        renderSettings(nextSettings);
        return;
      }

      savingDictationBar = true;
      renderSettings(nextSettings);
      messageEl.classList.remove("error");
      messageEl.textContent = "Saving…";

      try {
        const saved = await invoke("save_dictation_bar_settings", {
          barPosition: nextPosition,
          accentColor: nextAccent,
          barDisplay: nextDisplay
        });
        savingDictationBar = false;
        renderSettings(saved);
        messageEl.textContent = "Saved.";
      } catch (error) {
        savingDictationBar = false;
        renderSettings(previousSettings);
        messageEl.classList.add("error");
        messageEl.textContent = String(error);
      }
    }
