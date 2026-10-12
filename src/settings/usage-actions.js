    async function loadUsage() {
      const invoke = tauriInvoke();
      if (!invoke) {
        renderUsage(fallbackUsage);
        return;
      }

      try {
        renderUsage(await invoke("get_usage_summary"));
      } catch (error) {
        renderUsage(latestUsage, String(error), true);
      }
    }

    async function saveUsage(command, args, confirmedMessage) {
      const invoke = tauriInvoke();
      if (!invoke || savingUsage) return;

      savingUsage = true;
      renderUsage(latestUsage);

      let message = confirmedMessage;
      let isError = false;
      try {
        renderUsage(await invoke(command, args), message);
      } catch (error) {
        message = String(error);
        isError = true;
      }

      savingUsage = false;
      renderUsage(latestUsage, message, isError);
    }

    function setUsageStoring(enabled) {
      if (!enabled) {
        pendingUsageConfirm = "stop-storing";
        renderUsage(latestUsage);
        return;
      }
      pendingUsageConfirm = null;
      saveUsage("set_usage_storing", { enabled });
    }

    function cancelUsageConfirm() {
      pendingUsageConfirm = null;
      renderUsage(latestUsage);
    }

    async function acceptUsageConfirm() {
      const pending = pendingUsageConfirm;
      pendingUsageConfirm = null;
      if (pending === "stop-storing") {
        await saveUsage("set_usage_storing", { enabled: false });
        return;
      }
      if (pending === "redo") await redoTypingChallenges();
    }

    function saveTypingEstimate() {
      const raw = document.getElementById("usage-estimate-input").value.trim();
      if (raw === "") {
        saveUsage("set_typing_estimate", { estimate: null });
        return;
      }

      const estimate = Number(raw);
      if (!Number.isInteger(estimate)) {
        renderUsage(latestUsage, "Enter a whole number of words per minute.", true);
        return;
      }
      saveUsage("set_typing_estimate", { estimate });
    }

    async function openTypingChallenge() {
      const invoke = tauriInvoke();
      if (!invoke) return;

      try {
        await invoke("open_typing_challenge");
      } catch (error) {
        renderUsage(latestUsage, String(error), true);
      }
    }

    function askRedoTypingChallenges() {
      pendingUsageConfirm = "redo";
      renderUsage(latestUsage);
    }

    async function redoTypingChallenges() {
      const invoke = tauriInvoke();
      if (!invoke) return;

      try {
        await invoke("redo_typing_challenges");
        await loadUsage();
        await openTypingChallenge();
      } catch (error) {
        renderUsage(latestUsage, String(error), true);
      }
    }

    async function revealModel() {
      const invoke = tauriInvoke();
      if (!invoke) return;

      try {
        await invoke("reveal_model_location");
      } catch (error) {
        const messageEl = document.getElementById("model-message");
        messageEl.classList.add("error");
        messageEl.textContent = String(error);
      }
    }

