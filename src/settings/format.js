    function stateWord(item) {
      if (item.ready) return "ready";
      return item.required ? "action needed" : "optional";
    }

    function formatMb(bytes) {
      return (bytes / 1024 / 1024).toFixed(0);
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

