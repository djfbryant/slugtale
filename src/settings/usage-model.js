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
