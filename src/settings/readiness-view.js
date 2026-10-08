    function readinessRow(item) {
      const row = document.createElement("li");
      row.className = "item";
      row.dataset.ready = String(item.ready);
      row.dataset.required = String(item.required);

      const icon = document.createElement("span");
      icon.className = "icon";
      icon.innerHTML = iconSvg(item.ready ? "check" : item.required ? "alert" : "minus", 12);

      const copy = document.createElement("div");
      copy.className = "copy";

      const label = document.createElement("strong");
      label.textContent = item.label;

      const srState = document.createElement("span");
      srState.className = "sr-only";
      srState.textContent = ` — ${stateWord(item)}`;
      label.append(srState);
      copy.append(label);

      // The report may carry a reason only the backend knows — which engine
      // this build compiled in, which assets are installed. Prefer it over the
      // static copy, which cannot know any of that.
      const detail = item.detail || readinessGuidance(item.id);
      if (detail) {
        const small = document.createElement("small");
        small.textContent = detail;
        copy.append(small);
      }

      const action = readinessAction(item.id);
      if (action) {
        const button = document.createElement("button");
        button.type = "button";
        button.innerHTML = `${iconSvg("external", 14)}<span>${action.label}</span>`;
        button.addEventListener("click", () => openReadinessAction(item.id));
        row.append(icon, copy, button);
      } else {
        row.append(icon, copy);
      }
      return row;
    }

    // The problems on the pane in view, above the controls that fix them.
    function renderReadinessList(report) {
      const list = document.getElementById("readiness-list");
      const items = blockersOf(report).filter(
        (item) => item.pane === activePane && !PERMISSION_ITEMS.includes(item.id)
      );
      list.hidden = items.length === 0;
      list.replaceChildren(...items.map(readinessRow));
    }

    // The permissions are a fact about this machine, so their rows always say
    // where they stand, and offer the way to system settings when not granted.
    function renderPermissions(report) {
      PERMISSION_ITEMS.forEach((id) => {
        const item = report.items.find((entry) => entry.id === id);
        const row = document.getElementById(`permission-${id}-row`);
        const state = document.getElementById(`permission-${id}-state`);
        const control = document.getElementById(`permission-${id}-control`);
        row.hidden = !item;
        if (!item) return;

        row.dataset.issue = String(item.required && !item.ready);
        state.textContent = item.ready ? "Allowed." : item.detail || readinessGuidance(id);

        const action = readinessAction(id);
        if (item.ready) {
          const chip = document.createElement("span");
          chip.className = "chip";
          chip.textContent = "Allowed";
          control.replaceChildren(chip);
        } else if (action) {
          const button = document.createElement("button");
          button.type = "button";
          button.innerHTML = `${iconSvg("external", 14)}<span>${action.label}</span>`;
          button.addEventListener("click", () => openReadinessAction(id));
          control.replaceChildren(button);
        } else {
          control.replaceChildren();
        }
      });
    }

    function render(report) {
      latestReport = report;
      const ready = report.dictation_available;
      const status = document.getElementById("overall-status");

      status.dataset.ready = String(ready);
      document.getElementById("overall-status-icon").innerHTML = iconSvg(ready ? "check" : "alert", 15);
      document.getElementById("overall-status-text").textContent =
        ready ? "Ready to dictate" : "Not ready to dictate";

      const notReady = (id) => blockersOf(report).some((item) => item.id === id);
      document.getElementById("hotkey-row").dataset.issue = String(notReady("hotkey"));
      document.getElementById("model-row").dataset.issue = String(notReady("local_model"));

      renderBadges(report);
      renderPermissions(report);

      // The first report opens the window on its first problem, so the fix is
      // one look away. Later reports leave the user where they are.
      const firstProblem = blockersOf(report)[0];
      if (!paneChosen) {
        paneChosen = true;
        if (firstProblem?.pane) {
          selectPane(firstProblem.pane);
          return;
        }
      }
      renderReadinessList(report);
    }

    function renderAppearance() {
      const dark = Boolean(window.matchMedia?.("(prefers-color-scheme: dark)").matches);
      document.getElementById("appearance-now").innerHTML =
        `${iconSvg(dark ? "moon" : "sun", 14)}<span>Now ${dark ? "dark" : "light"}</span>`;
    }

