    function tauriCore() {
      return window.__TAURI__ && window.__TAURI__.core;
    }

    function tauriInvoke() {
      const core = tauriCore();
      return core && core.invoke;
    }

    function tauriEvent() {
      return window.__TAURI__ && window.__TAURI__.event && window.__TAURI__.event.listen;
    }

    function makeChannel(onMessage) {
      const core = tauriCore();
      if (!core || typeof core.Channel !== "function") return null;
      const channel = new core.Channel();
      channel.onmessage = onMessage;
      return channel;
    }

    function delay(ms) {
      return new Promise((resolve) => setTimeout(resolve, ms));
    }

    // The one entry point every page uses to reach a Tauri command. It no-ops
    // when the Tauri bridge is absent (a browser preview, a unit test's fake)
    // rather than throwing into the caller, and it logs a failure rather than
    // letting a rejected command surface as an unhandled rejection. The
    // Dictation Bar and Typing Challenge used to each define their own copy of
    // this; there is one now.
    function invoke(command, args) {
      const core = tauriCore();
      if (!core || typeof core.invoke !== "function") return Promise.resolve();
      return core.invoke(command, args).catch((error) => {
        console.error(command + " failed", error);
      });
    }

