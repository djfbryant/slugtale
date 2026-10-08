    function renderModel(status, message, isError = false) {
      const path = document.getElementById("model-path");
      const state = document.getElementById("model-state");
      const name = document.getElementById("model-name");
      const messageEl = document.getElementById("model-message");
      const downloadButton = document.getElementById("download-model-button");
      const deleteButton = document.getElementById("delete-model-button");
      const size = status.bytes ? `${formatMb(status.bytes)} MB` : null;

      name.textContent = status.id;
      path.textContent = status.path;
      state.dataset.present = String(status.present);
      state.textContent = status.present
        ? `Installed${size ? ` · ${size}` : ""}`
        : "Not downloaded";
      downloadButton.disabled = status.present || downloading;
      downloadButton.hidden = status.present && !downloading;
      deleteButton.disabled = !status.present || downloading;
      document.getElementById("model-location-row").hidden = !status.present;
      document.getElementById("model-delete-row").hidden = !status.present;
      messageEl.classList.toggle("error", isError);
      messageEl.textContent = message || (status.present
        ? "Captured audio and transcriptions stay on this machine."
        : `Download ${status.id} before transcription can run.`);
    }

    function showProgress(progress) {
      const wrap = document.getElementById("model-progress");
      const bar = wrap.querySelector(".progress-bar");
      const messageEl = document.getElementById("model-message");
      wrap.hidden = false;
      messageEl.classList.remove("error");

      if (progress.total) {
        const pct = Math.min(100, Math.round((progress.downloaded / progress.total) * 100));
        wrap.classList.remove("indeterminate");
        bar.style.width = `${pct}%`;
        messageEl.textContent =
          `Downloading base.en… ${pct}% · ${formatMb(progress.downloaded)} / ${formatMb(progress.total)} MB`;
      } else {
        wrap.classList.add("indeterminate");
        bar.style.removeProperty("width");
        messageEl.textContent = `Downloading base.en… ${formatMb(progress.downloaded)} MB`;
      }
    }

    function hideProgress() {
      const wrap = document.getElementById("model-progress");
      wrap.hidden = true;
      wrap.classList.remove("indeterminate");
      wrap.querySelector(".progress-bar").style.width = "0";
    }

