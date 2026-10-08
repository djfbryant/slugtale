    // The order the engine list reads in is a domain fact, not a view detail: the
    // selected engine first, then the ones another click of the radio can switch
    // to (alphabetically by the name shown), then any engine that cannot run right
    // now. Kept here, away from the DOM builder, so the rule can be reasoned about
    // and tested on its own — and so a new engine is usable without touching
    // `TranscriptionEngine::ALL`, whose order the Settings File relies on.
    function orderEngines(engines) {
      return [...engines].sort((a, b) => {
        if (a.is_primary !== b.is_primary) return a.is_primary ? -1 : 1;
        if (Boolean(a.unavailable_reason) !== Boolean(b.unavailable_reason)) {
          return a.unavailable_reason ? 1 : -1;
        }
        return a.display_name.localeCompare(b.display_name);
      });
    }
