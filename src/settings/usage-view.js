    function renderUsageSpan(prefix, saved, counts, hasBaseline) {
      document.getElementById(`usage-span-${prefix}`).dataset.baseline = String(hasBaseline);
      document.getElementById(`usage-${prefix}-saved`).textContent = saved;
      document.getElementById(`usage-${prefix}-counts`).textContent = counts;
    }

    function renderUsage(usage, message, isError = false) {
      latestUsage = usage;
      const model = usageModel(usage);

      document.getElementById("usage-store-toggle").checked = model.storing;
      document.getElementById("usage-store-toggle").disabled = savingUsage;

      const confirmBlock = document.getElementById("usage-confirm");
      const confirmation = USAGE_CONFIRMATIONS[pendingUsageConfirm];
      confirmBlock.hidden = !confirmation;
      if (confirmation) {
        document.getElementById("usage-confirm-text").textContent = confirmation.text;
        document.getElementById("usage-confirm-accept").textContent = confirmation.accept;
      }

      const hero = document.getElementById("usage-hero");
      hero.dataset.baseline = String(model.hasBaseline);
      document.getElementById("usage-hero-value").textContent = model.heroValue;

      const empty = document.getElementById("usage-empty");
      if (!model.storing) {
        empty.hidden = false;
        empty.textContent =
          "Nothing is being stored. Turn on counting below and Slugtale will start counting from then on — dictations before that are not kept. You can still measure your typing speed.";
      } else if (!model.anyCounts) {
        empty.hidden = false;
        empty.textContent = "No dictations counted yet.";
      } else {
        empty.hidden = true;
      }

      hero.hidden = !model.storing;
      document.getElementById("usage-spans").hidden = !model.storing;
      renderUsageSpan("today", model.todaySaved, model.todayCounts, model.hasBaseline);
      renderUsageSpan("week", model.weekSaved, model.weekCounts, model.hasBaseline);
      document.getElementById("usage-all-counts").textContent = model.allCounts;

      const heroNote = document.getElementById("usage-hero-note");
      heroNote.hidden = model.hasBaseline;
      heroNote.textContent = model.hasBaseline
        ? ""
        : "Slugtale needs to know how fast you type before it can say what dictating saved you.";

      const baselineButton = document.getElementById("usage-baseline-button");
      document.getElementById("usage-hero-action").hidden = model.measured;
      baselineButton.disabled = savingUsage;
      baselineButton.textContent = model.baselineButtonLabel;

      document.getElementById("usage-baseline-state").textContent = model.baselineStateText;
      document.getElementById("usage-redo-button").hidden = model.completedChallenges === 0;
      document.getElementById("usage-redo-button").disabled = savingUsage;

      const estimateInput = document.getElementById("usage-estimate-input");
      if (document.activeElement !== estimateInput) {
        estimateInput.value = model.estimateValue;
      }
      estimateInput.disabled = model.measured || savingUsage;
      document.getElementById("usage-estimate-save").disabled = model.measured || savingUsage;
      document.getElementById("usage-estimate-clear").disabled =
        model.measured || savingUsage || !model.hasEstimate;
      document.getElementById("usage-estimate-row").hidden = model.measured;

      const messageEl = document.getElementById("usage-message");
      messageEl.classList.toggle("error", Boolean(isError));
      messageEl.textContent = message
        || (model.wpm === null || model.wpm === undefined
          ? "Time saved is worked out from your counts and your typing speed. It is never stored as a number."
          : "Counts stay on this machine. Time saved is worked out from them, so it moves if you measure your typing again.");
    }
