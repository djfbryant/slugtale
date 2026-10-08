    function plural(count, word) {
      return `${count} ${word}${count === 1 ? "" : "s"}`;
    }

    // Time Saved arrives already worded from the backend — "About 12 min" — so
    // there is one place that decides how it reads. `null` is the hole: no
    // Typing Baseline, so no honest number to print.
    function renderUsageSpan(prefix, span, hasBaseline) {
      document.getElementById(`usage-span-${prefix}`).dataset.baseline = String(hasBaseline);
      document.getElementById(`usage-${prefix}-saved`).textContent = span.time_saved || "—";
      document.getElementById(`usage-${prefix}-counts`).textContent =
        `${plural(span.dictations, "dictation")} · ${plural(span.words, "word")}`;
    }

    function renderUsage(usage, message, isError = false) {
      latestUsage = usage;

      const hasBaseline = usage.measured_wpm !== null || usage.typed_estimate !== null;
      const wpm = usage.measured_wpm !== null ? usage.measured_wpm : usage.typed_estimate;
      const storing = Boolean(usage.store_usage);
      const anyDays = usage.all_time.dictations > 0 || usage.all_time.words > 0;

      // The toggle always shows what is actually stored, so an unconfirmed
      // flick of it does not leave the switch lying about the state.
      document.getElementById("usage-store-toggle").checked = storing;
      document.getElementById("usage-store-toggle").disabled = savingUsage;

      const confirmBlock = document.getElementById("usage-confirm");
      const confirmation = USAGE_CONFIRMATIONS[pendingUsageConfirm];
      confirmBlock.hidden = !confirmation;
      if (confirmation) {
        document.getElementById("usage-confirm-text").textContent = confirmation.text;
        document.getElementById("usage-confirm-accept").textContent = confirmation.accept;
      }

      const hero = document.getElementById("usage-hero");
      hero.dataset.baseline = String(hasBaseline);
      document.getElementById("usage-hero-value").textContent = usage.all_time.time_saved || "—";

      // Three empty states, and they say different things because the user is in
      // three different situations (ADR-0025).
      const empty = document.getElementById("usage-empty");
      if (!storing) {
        empty.hidden = false;
        empty.textContent =
          "Nothing is being stored. Turn on counting below and Slugtale will start counting from then on — dictations before that are not kept. You can still measure your typing speed.";
      } else if (!anyDays) {
        empty.hidden = false;
        empty.textContent = "No dictations counted yet.";
      } else {
        empty.hidden = true;
      }

      // With storing off the numbers are all zero, so showing zeroed cards would
      // only imply the user had a quiet week.
      hero.hidden = !storing;
      document.getElementById("usage-spans").hidden = !storing;
      renderUsageSpan("today", usage.today, hasBaseline);
      renderUsageSpan("week", usage.this_week, hasBaseline);
      document.getElementById("usage-all-counts").textContent =
        `${plural(usage.all_time.dictations, "dictation")} · ${plural(usage.all_time.words, "word")}`;

      const heroNote = document.getElementById("usage-hero-note");
      heroNote.hidden = hasBaseline;
      heroNote.textContent = hasBaseline
        ? ""
        : "Slugtale needs to know how fast you type before it can say what dictating saved you.";

      // The take-the-baseline action stays until the three challenges are done;
      // after that Redo on the row below is the way back in.
      const baselineButton = document.getElementById("usage-baseline-button");
      const measured = usage.measured_wpm !== null;
      document.getElementById("usage-hero-action").hidden = measured;
      baselineButton.disabled = savingUsage;
      baselineButton.textContent = usage.completed_challenges > 0
        ? `Continue typing challenge (${usage.completed_challenges} of ${usage.challenge_count})`
        : "Measure my typing speed";

      const state = document.getElementById("usage-baseline-state");
      if (measured) {
        state.textContent = `${usage.measured_wpm} words per minute, measured over ${usage.challenge_count} typing challenges.`;
      } else if (usage.typed_estimate !== null) {
        state.textContent = `${usage.typed_estimate} words per minute, your estimate. Take the challenge to measure it.`;
      } else if (usage.completed_challenges > 0) {
        state.textContent = `${usage.completed_challenges} of ${usage.challenge_count} typing challenges done.`;
      } else {
        state.textContent = "Not measured yet.";
      }
      document.getElementById("usage-redo-button").hidden = usage.completed_challenges === 0;
      document.getElementById("usage-redo-button").disabled = savingUsage;

      // Once measured, the estimate cannot be typed over the measurement, so the
      // field is disabled rather than silently ignoring what is typed into it.
      const estimateInput = document.getElementById("usage-estimate-input");
      if (document.activeElement !== estimateInput) {
        estimateInput.value = usage.typed_estimate === null ? "" : String(usage.typed_estimate);
      }
      estimateInput.disabled = measured || savingUsage;
      document.getElementById("usage-estimate-save").disabled = measured || savingUsage;
      document.getElementById("usage-estimate-clear").disabled =
        measured || savingUsage || usage.typed_estimate === null;
      document.getElementById("usage-estimate-row").hidden = measured;

      const messageEl = document.getElementById("usage-message");
      messageEl.classList.toggle("error", Boolean(isError));
      messageEl.textContent = message
        || (wpm === null || wpm === undefined
          ? "Time saved is worked out from your counts and your typing speed. It is never stored as a number."
          : "Counts stay on this machine. Time saved is worked out from them, so it moves if you measure your typing again.");
    }

