    const ACCENTS = {
      red: ["#ff5a52", "rgba(255, 90, 82, 0.3)", "rgba(255, 90, 82, 0.16)"],
      amber: ["#f5a524", "rgba(245, 165, 36, 0.3)", "rgba(245, 165, 36, 0.16)"],
      green: ["#35c76a", "rgba(53, 199, 106, 0.3)", "rgba(53, 199, 106, 0.16)"],
      blue: ["#4c9ffe", "rgba(76, 159, 254, 0.3)", "rgba(76, 159, 254, 0.16)"],
      violet: ["#a78bfa", "rgba(167, 139, 250, 0.3)", "rgba(167, 139, 250, 0.16)"],
      graphite: ["#9aa4ad", "rgba(154, 164, 173, 0.3)", "rgba(154, 164, 173, 0.16)"]
    };
    const PHASES = { recording: "Recording", transcribing: "Transcribing…" };
    const POINTER_POLL_MS = 100;
    const VOICE_LEVEL = 0.08;
    const VOICE_HOLD_MS = 700;
    const LEVEL_INTERVAL_MS = 33;

    const VIEW_WIDTH = 160;
    const VIEW_MIDDLE = 13;
    const HISTORY = 72;
    const ATTACK = 0.55;
    const RELEASE = 0.21;
    const FLOOR = 0.7;
    const FLOW_DEPTH = 0.19;
    const FLOW_RATE = 0.00183;

    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const halo = document.getElementById("halo");
    const label = document.getElementById("label");
    const elapsed = document.getElementById("elapsed");
    const envelope = document.getElementById("envelope");

    function stop() {
      invoke("dictation_event", { event: "stop" });
    }

    function cancel() {
      invoke("dictation_event", { event: "cancel" });
    }

    let phase = "recording";
    let phaseStart = Date.now();
    let pointerOver = false;
    let pointerPoll = null;
    let targetLevel = 0;
    let renderedLevel = 0;
    let lastLevelAt = 0;
    let voiceUntil = 0;
    let history = new Array(HISTORY).fill(0);
    let follower = 0;
    let drift = 0;
    let lastFrameAt = Date.now();

    function expansionReason(now) {
      if (pointerOver) return "hover";
      if (phase === "transcribing") return "transcribing";
      if (now < voiceUntil) return "voice";
      return "none";
    }

    function renderExpansion(reason) {
      document.body.dataset.reason = reason;
      document.body.dataset.expanded = String(reason !== "none");
    }

    function setPhase(next, startedAt) {
      phase = PHASES[next] ? next : "recording";
      phaseStart = typeof startedAt === "number" ? startedAt : Date.now();
      document.body.dataset.phase = phase;
      label.textContent = PHASES[phase];
      renderExpansion(expansionReason(Date.now()));
    }

    function setAppearance(appearance) {
      const settings = appearance || {};
      const position = BAR_POSITIONS.indexOf(settings.position) >= 0
        ? settings.position
        : BAR_POSITIONS[0];
      const accent = ACCENTS[settings.accent] || ACCENTS.red;

      document.body.dataset.position = position;
      document.documentElement.style.setProperty("--accent", accent[0]);
      document.documentElement.style.setProperty("--accent-soft", accent[1]);
      document.documentElement.style.setProperty("--accent-bed", accent[2]);
    }

    function setAudioLevel(level) {
      const value = Number.isFinite(level) ? Math.max(0, Math.min(1, level)) : 0;
      targetLevel = value;
      lastLevelAt = Date.now();

      follower += (value - follower) * (value > follower ? ATTACK : RELEASE);
      if (reduceMotion) {
        history.fill(follower);
      } else {
        history.push(follower);
        history.shift();
      }

      if (value > VOICE_LEVEL) voiceUntil = lastLevelAt + VOICE_HOLD_MS;
    }

    function resetWave() {
      history = new Array(HISTORY).fill(0);
      follower = 0;
      drift = 0;
      targetLevel = 0;
      renderedLevel = 0;
      voiceUntil = 0;
    }

    function splinePath(points, start) {
      let d = (start === false ? "L" : "M") + points[0][0].toFixed(2) + "," + points[0][1].toFixed(2);
      for (let i = 0; i < points.length - 1; i += 1) {
        const before = points[Math.max(0, i - 1)];
        const from = points[i];
        const to = points[i + 1];
        const after = points[Math.min(points.length - 1, i + 2)];
        d += " C" + (from[0] + (to[0] - before[0]) / 6).toFixed(2) + "," + (from[1] + (to[1] - before[1]) / 6).toFixed(2) +
          " " + (to[0] - (after[0] - from[0]) / 6).toFixed(2) + "," + (to[1] - (after[1] - from[1]) / 6).toFixed(2) +
          " " + to[0].toFixed(2) + "," + to[1].toFixed(2);
      }
      return d;
    }

    function edge(value, age, wobble) {
      return Math.max(FLOOR, value * 11 * (0.55 + 0.45 * age) * (1 + value * wobble));
    }

    function renderWave(now) {
      const step = reduceMotion
        ? 0
        : Math.max(0, Math.min(1, (now - lastLevelAt) / LEVEL_INTERVAL_MS));
      const spacing = VIEW_WIDTH / (HISTORY - 2);
      const top = [];
      const bottom = [];

      for (let i = 0; i < HISTORY; i += 1) {
        const x = VIEW_WIDTH - ((HISTORY - 1 - i) - step) * spacing;
        const age = i / (HISTORY - 1);
        const topWobble = reduceMotion ? 0 : FLOW_DEPTH * Math.sin(age * 5.5 - drift);
        const bottomWobble = reduceMotion ? 0 : FLOW_DEPTH * Math.sin(age * 4.2 - drift * 0.75 + 2.3);
        top.push([x, VIEW_MIDDLE - edge(history[i], age, topWobble)]);
        bottom.push([x, VIEW_MIDDLE + edge(history[i], age, bottomWobble)]);
      }

      envelope.setAttribute("d", splinePath(top) + " " + splinePath(bottom.reverse(), false) + " Z");
    }

    async function pollPointer() {
      const expanded = document.body.dataset.expanded === "true";
      const over = await invoke("dictation_bar_pointer_over", { expanded });
      pointerOver = over === true;
      renderExpansion(expansionReason(Date.now()));
    }

    function setVisible(visible) {
      if (visible && pointerPoll === null) {
        resetWave();
        pointerPoll = setInterval(pollPointer, POINTER_POLL_MS);
        return;
      }
      if (!visible && pointerPoll !== null) {
        clearInterval(pointerPoll);
        pointerPoll = null;
        pointerOver = false;
        renderExpansion(expansionReason(Date.now()));
      }
    }

    function clockLabel(now) {
      const seconds = Math.max(0, Math.floor((now - phaseStart) / 1000));
      return Math.floor(seconds / 60) + ":" + String(seconds % 60).padStart(2, "0");
    }

    function renderFrame(_timestamp, now) {
      const at = typeof now === "number" ? now : Date.now();
      const sinceLastFrame = Math.min(100, at - lastFrameAt);
      lastFrameAt = at;

      renderedLevel += (targetLevel - renderedLevel) * 0.28;
      halo.style.transform = "scale(" + (0.7 + renderedLevel * 0.42).toFixed(3) + ")";
      halo.style.opacity = (0.3 + renderedLevel * 0.5).toFixed(2);

      if (!reduceMotion) drift += FLOW_RATE * sinceLastFrame;

      const reason = expansionReason(at);
      renderExpansion(reason);
      if (reason === "voice") renderWave(at);

      elapsed.textContent = clockLabel(at);
    }

    function animate(timestamp) {
      renderFrame(timestamp);
      requestAnimationFrame(animate);
    }

    document.getElementById("stop").addEventListener("click", stop);
    document.getElementById("cancel").addEventListener("click", cancel);

    document.querySelector(".bar").addEventListener("mouseleave", pollPointer);

    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && phase !== "transcribing") {
        event.preventDefault();
        cancel();
      }
    });

    setPhase("recording");
    setAudioLevel(0);
    resetWave();
    requestAnimationFrame(animate);

    const listen = tauriEvent();
    if (listen) {
      listen("dictation-phase", (event) => setPhase(event.payload));
      listen("dictation-appearance", (event) => setAppearance(event.payload));
      listen("dictation-audio-level", (event) => setAudioLevel(event.payload));
      listen("dictation-visibility", (event) => setVisible(event.payload === true));
    }
