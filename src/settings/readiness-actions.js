    async function loadReadiness(options = {}) {
      if (options.background && openingReadinessAction) return;

      const invoke = tauriInvoke();
      if (!invoke) {
        render(fallbackReport);
        return;
      }

      try {
        const report = await invoke("get_settings_readiness");
        if (options.background && openingReadinessAction) return;
        render(report);
      } catch (error) {
        console.error("Could not load readiness", error);
        if (options.background && openingReadinessAction) return;
        render(fallbackReport);
      }
    }

    async function pollReadinessUntilReady(itemId, attempts = 12) {
      const invoke = tauriInvoke();
      if (!invoke) return;

      for (let attempt = 0; attempt < attempts; attempt += 1) {
        await delay(1000);
        const report = await invoke("get_settings_readiness");
        render(report);

        const item = report.items.find((entry) => entry.id === itemId);
        if (item?.ready) {
          renderSettings(currentSettings, "Permission is ready.");
          return;
        }
      }

      renderSettings(
        currentSettings,
        PLATFORM === "linux"
          ? "Still not ready. Connect a microphone or switch to an X11 session, then reopen this window."
          : "Still not granted. Grant access in your system settings, then reopen this window."
      );
    }

    async function openReadinessAction(itemId) {
      const invoke = tauriInvoke();
      const action = readinessAction(itemId);
      if (!invoke || !action || openingReadinessAction) return;

      openingReadinessAction = true;
      try {
        await invoke(action.command);
        renderSettings(
          currentSettings,
          "Checking permission. If a system settings panel opens, make the change there and return here."
        );
        await pollReadinessUntilReady(itemId);
      } catch (error) {
        renderSettings(currentSettings, String(error), true);
      } finally {
        openingReadinessAction = false;
      }
    }

