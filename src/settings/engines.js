
    // One line of a Transcription Engine's compliance surface: model identity,
    // pinned revision, size, source, licence, attribution, and modifications.
    // Built from `EngineMetadata` rather than retyped, so the CC BY 4.0 wording
    // Parakeet requires can only ever change in one place (parakeet.rs).
    function engineDetailLines(engine) {
      const meta = engine.metadata;
      const lines = [];

      const sizeText = engine.assets.installed_bytes
        ? `${formatMb(engine.assets.installed_bytes)} MB installed`
        : meta.approximate_bytes
          ? `~${formatMb(meta.approximate_bytes)} MB`
          : null;
      const modelLine = document.createElement("p");
      modelLine.className = "engine-detail-line";
      modelLine.textContent = [`Model: ${meta.model_id}`, meta.revision, sizeText]
        .filter(Boolean)
        .join(" · ");
      lines.push(modelLine);

      if (meta.source_url) {
        const sourceLine = document.createElement("p");
        sourceLine.className = "engine-detail-line";
        const link = document.createElement("a");
        link.href = meta.source_url;
        link.target = "_blank";
        link.rel = "noreferrer";
        link.textContent = "View source";
        sourceLine.append("Source: ", link);
        lines.push(sourceLine);
      }

      const licenceLine = document.createElement("p");
      licenceLine.className = "engine-detail-line";
      const licenceLink = document.createElement("a");
      licenceLink.href = meta.license_url;
      licenceLink.target = "_blank";
      licenceLink.rel = "noreferrer";
      licenceLink.textContent = meta.license;
      licenceLine.append("Licence: ", licenceLink);
      lines.push(licenceLine);

      if (meta.attribution) {
        const attributionLine = document.createElement("p");
        attributionLine.className = "engine-detail-line";
        attributionLine.textContent = meta.attribution;
        lines.push(attributionLine);
      }

      if (meta.modifications) {
        const modificationsLine = document.createElement("p");
        modificationsLine.className = "engine-detail-line";
        modificationsLine.textContent = meta.modifications;
        lines.push(modificationsLine);
      }

      if (meta.system_managed) {
        // Apple SpeechTranscriber only: state plainly that macOS owns these
        // assets so nothing here can be read as Slugtale bundling or shipping
        // Apple's model.
        const systemLine = document.createElement("p");
        systemLine.className = "engine-detail-line";
        systemLine.textContent =
          `Available only on ${meta.supported_platforms}. Installed and updated by macOS — ` +
          "Slugtale does not bundle, download, or redistribute these assets.";
        lines.push(systemLine);
      }

      return lines;
    }

    function buildEngineItem(engine) {
      const row = document.createElement("li");
      row.className = "engine-item";
      row.dataset.engine = engine.id;

      const head = document.createElement("div");
      head.className = "engine-head";

      const radio = document.createElement("input");
      radio.type = "radio";
      radio.name = "primary-engine";
      radio.value = engine.id;
      radio.checked = engine.is_primary;
      radio.disabled = savingEngineSelection || Boolean(engine.unavailable_reason);
      radio.setAttribute("aria-label", `Make ${engine.display_name} the primary transcription engine`);
      radio.addEventListener("change", () => {
        if (radio.checked) selectPrimaryEngine(engine.id);
      });

      const name = document.createElement("span");
      name.className = "engine-name";
      name.textContent = engine.display_name;

      const pill = document.createElement("span");
      pill.className = "engine-availability";
      pill.dataset.available = String(!engine.unavailable_reason);
      pill.textContent = engine.unavailable_reason ? "Unavailable" : "Available";

      head.append(radio, name, pill);
      row.append(head);

      // What the engine is good at, asked of the engine itself rather than
      // retyped here — so a fifth engine brings its own copy without a
      // JavaScript change.
      if (engine.metadata.capability) {
        const blurb = document.createElement("p");
        blurb.className = "engine-blurb";
        blurb.textContent = engine.metadata.capability;
        row.append(blurb);
      }

      if (engine.unavailable_reason) {
        const reason = document.createElement("p");
        reason.className = "engine-reason";
        reason.textContent = engine.unavailable_reason;
        row.append(reason);
      }

      const detail = document.createElement("div");
      detail.className = "engine-detail";
      detail.append(...engineDetailLines(engine));
      row.append(detail);

      const installing = installingEngine === engine.id;
      const actions = document.createElement("div");
      actions.className = "engine-actions";

      // Only a missing-assets engine earns an Install button (mirrors
      // `EngineUnavailable::is_user_resolvable` — an unsupported OS or a build
      // without the feature has nothing here to fix).
      if (engine.installable) {
        const installButton = document.createElement("button");
        installButton.type = "button";
        installButton.className = "primary";
        installButton.disabled = installingEngine !== null;
        installButton.textContent = installing ? "Installing…" : "Install";
        installButton.addEventListener("click", () => installEngine(engine.id));
        actions.append(installButton);
      }

      if (engine.assets.present) {
        const removeButton = document.createElement("button");
        removeButton.type = "button";
        removeButton.className = "danger";
        removeButton.disabled = installingEngine !== null;
        removeButton.textContent = "Remove";
        removeButton.addEventListener("click", () => removeEngine(engine.id));
        actions.append(removeButton);
      }

      if (actions.children.length) row.append(actions);

      if (installing && installProgress) {
        const wrap = document.createElement("div");
        wrap.className = "progress engine-progress";
        const bar = document.createElement("div");
        bar.className = "progress-bar";
        const pct = progressPercent(installProgress.downloaded, installProgress.total);
        if (pct === null) {
          wrap.classList.add("indeterminate");
        } else {
          bar.style.width = `${pct}%`;
        }
        wrap.append(bar);
        row.append(wrap);
      }

      return row;
    }

    function renderEngines(engines, message = "", isError = false) {
      latestEngines = engines;
      const list = document.getElementById("engine-list");
      const messageEl = document.getElementById("engine-message");

      // So the user reads in one glance what they would otherwise hunt for — the
      // selected engine first, then the engines they can switch to with another
      // click of the radio, then any engine that cannot run right now. The rule
      // lives in engine-model.js, out of this DOM builder.
      const sorted = orderEngines(engines);

      list.replaceChildren(...sorted.map(buildEngineItem));

      messageEl.classList.toggle("error", isError);
      messageEl.textContent =
        message || "Transcription runs entirely on this device. There is no cloud fallback.";
    }

    async function loadEngines(message = "", isError = false) {
      const invoke = tauriInvoke();
      if (!invoke) {
        renderEngines(fallbackEngines, message, isError);
        return;
      }

      try {
        renderEngines(await invoke("transcription_engines"), message, isError);
      } catch (error) {
        console.error("Could not load transcription engines", error);
        renderEngines(latestEngines, String(error), true);
      }
    }

    // The primary engine and Second Opinion mode are one Settings File update
    // (`set_transcription_engines`), so changing either one sends the pair —
    // the other rides along at its current value, same discipline as
    // `saveDictationBarSettings` above.
    async function saveEngineSettings({ primaryEngine, secondOpinion } = {}) {
      const invoke = tauriInvoke();
      const previousSettings = { ...currentSettings };
      const previousEngines = latestEngines;
      const nextPrimary = primaryEngine || currentSettings.primary_engine;
      const nextSecondOpinion = secondOpinion || currentSettings.second_opinion;
      const nextSettings = {
        ...currentSettings,
        primary_engine: nextPrimary,
        second_opinion: nextSecondOpinion
      };
      const optimisticEngines = latestEngines.map((engine) => ({
        ...engine,
        is_primary: engine.id === nextPrimary
      }));

      if (!invoke) {
        renderSettings(setSettings(nextSettings));
        renderEngines(optimisticEngines);
        return;
      }

      savingEngineSelection = true;
      renderSettings(setSettings(nextSettings));
      renderEngines(optimisticEngines, "Saving…");

      try {
        const saved = await invoke("set_transcription_engines", {
          primaryEngine: nextPrimary,
          secondOpinion: nextSecondOpinion
        });
        savingEngineSelection = false;
        renderSettings(setSettings(saved));
        await loadEngines(
          nextSecondOpinion === "automatic"
            ? "Saved. A second engine now runs only when the first result looks uncertain."
            : "Saved. Applies to your next dictation."
        );
      } catch (error) {
        savingEngineSelection = false;
        renderSettings(setSettings(previousSettings));
        renderEngines(previousEngines, String(error), true);
      }
    }

    function selectPrimaryEngine(engineId) {
      if (engineId === currentSettings.primary_engine) return;
      saveEngineSettings({ primaryEngine: engineId });
    }

    async function installEngine(engineId) {
      const invoke = tauriInvoke();
      if (!invoke || installingEngine) return;

      installingEngine = engineId;
      installProgress = { downloaded: 0, total: null };
      renderEngines(latestEngines, "Installing…");

      const channel = makeChannel((progress) => {
        installProgress = progress;
        renderEngines(latestEngines, "Installing…");
      });

      try {
        await invoke(
          "install_engine_assets",
          channel ? { engine: engineId, onProgress: channel } : { engine: engineId }
        );
        installingEngine = null;
        installProgress = null;
        await loadEngines("Installed. Ready to use.");
      } catch (error) {
        installingEngine = null;
        installProgress = null;
        await loadEngines(String(error), true);
      }
    }

    async function removeEngine(engineId) {
      const invoke = tauriInvoke();
      if (!invoke || installingEngine) return;

      try {
        await invoke("remove_engine_assets", { engine: engineId });
        await loadEngines("Removed.");
      } catch (error) {
        await loadEngines(String(error), true);
      }
    }

