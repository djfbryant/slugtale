    function readinessGuidance(id) {
      const copy = READINESS_COPY[id];
      if (!copy) return "";
      if (typeof copy.guidance === "string") return copy.guidance;
      return copy.guidance[PLATFORM] || "";
    }

    function readinessAction(id) {
      return READINESS_COPY[id]?.actions?.[PLATFORM] || null;
    }

    // One entry per sidebar section, in sidebar order. A readiness item names
    // the pane that shows it, so these ids are also the backend's
    // `ReadinessPane` wire names.
    const PANES = [
      { id: "shortcut", title: "Shortcut", icon: "keyboard", subtitle: "How you start and stop dictating." },
      { id: "transcription", title: "Transcription", icon: "cpu", subtitle: "Which on-device engine turns speech into text." },
      { id: "text", title: "Text & cleanup", icon: "wand", subtitle: "What your words look like when they land." },
      { id: "bar", title: "Dictation Bar", icon: "panel", subtitle: "The small bar you see while you speak." },
      { id: "usage", title: "Usage", icon: "gauge", subtitle: "Counts only, and only if you ask. Nothing is shared." },
      { id: "privacy", title: "Privacy & access", icon: "shield", subtitle: "What Slugtale may use on this machine." },
      { id: "general", title: "General", icon: "sliders", subtitle: "How Slugtale behaves in the background." }
    ];

    // The two OS permissions have a row of their own on the Privacy pane, so
    // the pane's problem list leaves them to it rather than saying it twice.
    const PERMISSION_ITEMS = ["microphone", "text_insertion"];

