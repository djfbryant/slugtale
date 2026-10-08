    function renderModel(status, message, isError = false) {
      latestModelStatus = status;
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

      // Name the model actually being fetched, from the status the pane rendered,
      // not a hardcoded id — a different model must not be labelled "base.en".
      const label = `Downloading ${latestModelStatus.id}…`;
      const pct = progressPercent(progress.downloaded, progress.total);
      if (pct === null) {
        wrap.classList.add("indeterminate");
        bar.style.removeProperty("width");
        messageEl.textContent = `${label} ${formatMb(progress.downloaded)} MB`;
      } else {
        wrap.classList.remove("indeterminate");
        bar.style.width = `${pct}%`;
        messageEl.textContent =
          `${label} ${pct}% · ${formatMb(progress.downloaded)} / ${formatMb(progress.total)} MB`;
      }
    }

    function hideProgress() {
      const wrap = document.getElementById("model-progress");
      wrap.hidden = true;
      wrap.classList.remove("indeterminate");
      wrap.querySelector(".progress-bar").style.width = "0";
    }
