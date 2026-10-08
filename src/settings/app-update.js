    let appUpdateState = { phase: "idle" };

    function renderAppUpdate() {
      const stateEl = document.getElementById("app-update-state");
      const releaseButton = document.getElementById("app-update-release-button");
      const checkButton = document.getElementById("app-update-check-button");
      const messageEl = document.getElementById("app-update-message");

      if (appUpdateState.phase === "checking") {
        stateEl.textContent = "Checking for updates…";
      } else if (appUpdateState.phase === "current") {
        stateEl.textContent = "Slugtale is up to date.";
      } else if (appUpdateState.phase === "available") {
        stateEl.textContent = `Version ${appUpdateState.version} is available.`;
      } else if (appUpdateState.phase === "error") {
        stateEl.textContent = "Slugtale could not check for updates.";
      } else {
        stateEl.textContent = "Checks happen only when you select Check now.";
      }
      checkButton.disabled = appUpdateState.phase === "checking";
      releaseButton.hidden = appUpdateState.phase !== "available";
      messageEl.classList.toggle("error", Boolean(appUpdateState.isError));
      messageEl.textContent = appUpdateState.message || "";
    }

    function setAppUpdateState(nextState) {
      appUpdateState = nextState;
      renderAppUpdate();
    }

    async function checkForAppUpdate() {
      const invoke = tauriInvoke();
      if (!invoke || appUpdateState.phase === "checking") return;

      setAppUpdateState({ phase: "checking" });

      try {
        const update = await invoke("check_for_app_update");
        setAppUpdateState(
          update.status === "available"
            ? { phase: "available", version: update.version }
            : { phase: "current" },
        );
      } catch (error) {
        setAppUpdateState({ phase: "error", message: String(error), isError: true });
      }
    }

    async function openAppUpdateRelease() {
      const invoke = tauriInvoke();
      if (!invoke || appUpdateState.phase !== "available") return;

      try {
        await invoke("open_app_update_release");
      } catch (error) {
        setAppUpdateState({
          phase: "available",
          version: appUpdateState.version,
          message: String(error),
          isError: true,
        });
      }
    }

