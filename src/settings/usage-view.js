    // Time Saved arrives already worded from the backend — "About 12 min" — so
    // there is one place that decides how it reads (see usage-model.js). This
    // file only paints that decided value; `null` is the hole: no Typing
    // Baseline, so no honest number to print.
    function renderUsageSpan(prefix, saved, counts, hasBaseline) {
      document.getElementById(`usage-span-${prefix}`).dataset.baseline = String(hasBaseline);
      document.getElementById(`usage-${prefix}-saved`).textContent = saved;
      document.getElementById(`usage-${prefix}-counts`).textContent = counts;
    }

    function renderUsage(usage, message, isError = false) {
      latestUsage = usage;
      const model = usageModel(usage);

      // The toggle always shows what is actually stored, so an unconfirmed
      // flick of it does not leave the switch lying about the state.
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

      // Three empty states, and they say different things because the user is in
      // three different situations (ADR-0025).
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

      // With storing off the numbers are all zero, so showing zeroed cards would
      // only imply the user had a quiet week.
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

      // The take-the-baseline action stays until the three challenges are done;
      // after that Redo on the row below is the way back in.
      const baselineButton = document.getElementById("usage-baseline-button");
      document.getElementById("usage-hero-action").hidden = model.measured;
      baselineButton.disabled = savingUsage;
      baselineButton.textContent = model.baselineButtonLabel;

      document.getElementById("usage-baseline-state").textContent = model.baselineStateText;
      document.getElementById("usage-redo-button").hidden = model.completedChallenges === 0;
      document.getElementById("usage-redo-button").disabled = savingUsage;

      // Once measured, the estimate cannot be typed over the measurement, so the
      // field is disabled rather than silently ignoring what is typed into it.
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
