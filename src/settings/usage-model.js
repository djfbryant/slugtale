    // The Usage pane is one command's answer turned into a screen. What the
    // answer MEANS — which states the counts put it in, what the take-the-challenge
    // button should say, whether Time Saved is a number or a hole — is derived
    // here, apart from the DOM, so `renderUsage` only paints and the derivations
    // can be tested on their own.
    function plural(count, word) {
      return `${count} ${word}${count === 1 ? "" : "s"}`;
    }

    function usageCountsText(span) {
      return `${plural(span.dictations, "dictation")} · ${plural(span.words, "word")}`;
    }

    function usageBaselineStateText(usage, measured) {
      if (measured) {
        return `${usage.measured_wpm} words per minute, measured over ${usage.challenge_count} typing challenges.`;
      }
      if (usage.typed_estimate !== null) {
        return `${usage.typed_estimate} words per minute, your estimate. Take the challenge to measure it.`;
      }
      if (usage.completed_challenges > 0) {
        return `${usage.completed_challenges} of ${usage.challenge_count} typing challenges done.`;
      }
      return "Not measured yet.";
    }

    // Everything `renderUsage` reads, decided once from the summary. Time Saved
    // arrives already worded from the backend — "About 12 min" — and `wpm === null`
    // is the hole: no Typing Baseline, so no honest number to print.
    function usageModel(usage) {
      const measured = usage.measured_wpm !== null;
      const wpm = measured ? usage.measured_wpm : usage.typed_estimate;
      return {
        hasBaseline: wpm !== null,
        wpm,
        measured,
        storing: Boolean(usage.store_usage),
        anyCounts: usage.all_time.dictations > 0 || usage.all_time.words > 0,
        heroValue: usage.all_time.time_saved || "—",
        todaySaved: usage.today.time_saved || "—",
        todayCounts: usageCountsText(usage.today),
        weekSaved: usage.this_week.time_saved || "—",
        weekCounts: usageCountsText(usage.this_week),
        allCounts: usageCountsText(usage.all_time),
        completedChallenges: usage.completed_challenges,
        challengeCount: usage.challenge_count,
        baselineButtonLabel: usage.completed_challenges > 0
          ? `Continue typing challenge (${usage.completed_challenges} of ${usage.challenge_count})`
          : "Measure my typing speed",
        baselineStateText: usageBaselineStateText(usage, measured),
        estimateValue: usage.typed_estimate === null ? "" : String(usage.typed_estimate),
        hasEstimate: usage.typed_estimate !== null
      };
    }
