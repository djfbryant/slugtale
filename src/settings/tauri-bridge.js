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

