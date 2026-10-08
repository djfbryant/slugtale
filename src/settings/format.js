    function stateWord(item) {
      if (item.ready) return "ready";
      return item.required ? "action needed" : "optional";
    }

    function formatMb(bytes) {
      return (bytes / 1024 / 1024).toFixed(0);
    }

    // One progress-percent rule, shared by the model download and the engine
    // install, so the two bars cannot round or clamp differently. `null` means
    // "no total known", which the bar draws as indeterminate.
    function progressPercent(downloaded, total) {
      if (!total) return null;
      return Math.min(100, Math.round((downloaded / total) * 100));
    }

    function keycapsHtml(hotkey) {
      if (!hotkey) return "";
      return hotkey
        .split("+")
        .map((key) => `<kbd>${HOTKEY_GLYPHS[key] || key}</kbd>`)
        .join("");
    }

    function blockersOf(report) {
      return report.items.filter((item) => item.required && !item.ready);
    }

