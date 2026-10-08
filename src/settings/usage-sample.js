
    // Shown only outside the desktop app (no Tauri bridge), so the pane is never
    // blank. Mirrors a fresh install: storing off, no baseline, nothing counted.
    const fallbackUsage = {
      store_usage: false,
      today: { dictations: 0, words: 0, time_saved: null },
      this_week: { dictations: 0, words: 0, time_saved: null },
      all_time: { dictations: 0, words: 0, time_saved: null },
      measured_wpm: null,
      typed_estimate: null,
      completed_challenges: 0,
      challenge_count: 3
    };

