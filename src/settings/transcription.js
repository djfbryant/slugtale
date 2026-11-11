    function saveTranscriptionSettings(speedProfile) {
      return saveSetting({
        command: "save_transcription_settings",
        args: { speedProfile },
        patch: { speed_profile: speedProfile },
        messageId: "transcription-message",
        successText: "Saved. Applies to your next dictation.",
        setPending: (pending) => { savingProfile = pending; }
      });
    }

    function saveSegmentPauseSettings(segmentPauseSecs) {
      return saveSetting({
        command: "save_segment_pause_settings",
        args: { segmentPauseSecs },
        patch: { segment_pause_secs: segmentPauseSecs },
        messageId: "transcription-message",
        successText: "Saved. Applies to your next dictation.",
        setPending: (pending) => { savingPause = pending; }
      });
    }

    async function saveSetting({ command, args, patch, messageId, successText, setPending }) {
      const invoke = tauriInvoke();
      const previousSettings = { ...currentSettings };
      const nextSettings = { ...currentSettings, ...patch };
      const messageEl = document.getElementById(messageId);

      if (!invoke) {
        renderSettings(setSettings(nextSettings));
        return;
      }

      setPending(true);
      renderSettings(setSettings(nextSettings));
      messageEl.classList.remove("error");
      messageEl.textContent = "Saving…";

      try {
        const saved = await invoke(command, args);
        setPending(false);
        renderSettings(setSettings(saved));
        messageEl.textContent = successText;
      } catch (error) {
        setPending(false);
        renderSettings(setSettings(previousSettings));
        messageEl.classList.add("error");
        messageEl.textContent = String(error);
      }
    }

    async function saveTranscriptCleanupSettings(cleanupMode) {
      await saveSetting({
        command: "save_transcript_cleanup_settings",
        args: { cleanupMode },
        patch: { transcript_cleanup: cleanupMode },
        messageId: "cleanup-message",
        successText: cleanupMode !== "basic" ? "Saved. Applies to your next dictation." : "Saved.",
        setPending: (pending) => { savingCleanup = pending; }
      });
    }

    async function saveDictationBarSettings({ barPosition, accentColor, barDisplay }) {
      const nextPosition = barPosition || currentSettings.bar_position;
      const nextAccent = accentColor || currentSettings.accent_color;
      const nextDisplay = barDisplay || currentSettings.bar_display;
      await saveSetting({
        command: "save_dictation_bar_settings",
        args: { barPosition: nextPosition, accentColor: nextAccent, barDisplay: nextDisplay },
        patch: { bar_position: nextPosition, accent_color: nextAccent, bar_display: nextDisplay },
        messageId: "dictation-bar-message",
        successText: "Saved.",
        setPending: (pending) => { savingDictationBar = pending; }
      });
    }
