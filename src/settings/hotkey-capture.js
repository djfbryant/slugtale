    function hotkeyKeyToken(event) {
      if (event.code.startsWith("Key")) return event.code.slice(3).toUpperCase();
      if (event.code.startsWith("Digit")) return event.code.slice(5);
      return event.code || event.key;
    }

    function hotkeyFromKeyboardEvent(event) {
      const modifierKeys = new Set(["Alt", "Control", "Meta", "Shift"]);
      if (modifierKeys.has(event.key)) return null;

      const modifiers = [];
      if (event.metaKey) modifiers.push("Cmd");
      if (event.ctrlKey) modifiers.push("Ctrl");
      if (event.altKey) modifiers.push("Alt");
      if (event.shiftKey) modifiers.push("Shift");

      if (modifiers.length === 0) {
        return { error: "Use a modifier key." };
      }

      return { hotkey: [...modifiers, hotkeyKeyToken(event)].join("+") };
    }

    function startHotkeyCapture() {
      capturingHotkey = true;
      renderSettings(currentSettings, "Press the shortcut you want to use.");
      document.getElementById("hotkey-input").focus();
    }

    function stopHotkeyCapture(message = "") {
      capturingHotkey = false;
      renderSettings(currentSettings, message);
    }

