    async function loadModelStatus() {
      const invoke = tauriInvoke();
      if (!invoke) {
        renderModel(fallbackModelStatus);
        return;
      }

      try {
        renderModel(await invoke("get_local_model_status"));
      } catch (error) {
        console.error("Could not load model status", error);
        renderModel(fallbackModelStatus, String(error), true);
      }
    }

    async function refreshSetup() {
      await Promise.all([
        loadReadiness(),
        loadModelStatus(),
        loadSettings(),
        loadDictationBarDisplays(),
        loadEngines(),
        loadUsage()
      ]);
    }

    function showMessageError(text) {
      const messageEl = document.getElementById("model-message");
      messageEl.classList.add("error");
      messageEl.textContent = text;
    }

    async function downloadModel() {
      const invoke = tauriInvoke();
      if (!invoke || downloading) return;

      downloading = true;
      const button = document.getElementById("download-model-button");
      button.disabled = true;
      document.getElementById("delete-model-button").disabled = true;
      setButton(button, "download", "Downloading…");
      showProgress({ downloaded: 0, total: null });

      const channel = makeChannel((progress) => showProgress(progress));

      try {
        const status = await invoke("download_local_model", channel ? { onProgress: channel } : {});
        downloading = false;
        hideProgress();
        setButton(button, "download", "Download");
        renderModel(status, "base.en is ready.");
        await loadReadiness();
      } catch (error) {
        downloading = false;
        hideProgress();
        setButton(button, "download", "Download");
        await loadModelStatus();
        showMessageError(String(error));
      }
    }

    async function deleteModel() {
      const invoke = tauriInvoke();
      if (!invoke || downloading) return;

      document.getElementById("delete-model-button").disabled = true;

      try {
        renderModel(await invoke("delete_local_model"), "Model deleted.");
        await loadReadiness();
      } catch (error) {
        await loadModelStatus();
        showMessageError(String(error));
      }
    }

