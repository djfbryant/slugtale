    function orderEngines(engines) {
      return [...engines].sort((a, b) => {
        if (a.is_primary !== b.is_primary) return a.is_primary ? -1 : 1;
        if (Boolean(a.unavailable_reason) !== Boolean(b.unavailable_reason)) {
          return a.unavailable_reason ? 1 : -1;
        }
        return a.display_name.localeCompare(b.display_name);
      });
    }
