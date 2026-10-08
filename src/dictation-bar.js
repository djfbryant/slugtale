    // The six-swatch palette. Accents arrive as names, never as colour values, so
    // nothing the backend sends is interpolated into CSS.
    const ACCENTS = {
      red: ["#ff5a52", "rgba(255, 90, 82, 0.3)", "rgba(255, 90, 82, 0.16)"],
      amber: ["#f5a524", "rgba(245, 165, 36, 0.3)", "rgba(245, 165, 36, 0.16)"],
      green: ["#35c76a", "rgba(53, 199, 106, 0.3)", "rgba(53, 199, 106, 0.16)"],
      blue: ["#4c9ffe", "rgba(76, 159, 254, 0.3)", "rgba(76, 159, 254, 0.16)"],
      violet: ["#a78bfa", "rgba(167, 139, 250, 0.3)", "rgba(167, 139, 250, 0.16)"],
      graphite: ["#9aa4ad", "rgba(154, 164, 173, 0.3)", "rgba(154, 164, 173, 0.16)"]
    };
    const PHASES = { recording: "Recording", transcribing: "Transcribing…" };
    // How often the backend is asked where the pointer is. The bar cannot watch
    // the pointer itself while it is letting clicks through to the app below.
    const POINTER_POLL_MS = 100;
    // The level the frontend treats as voice rather than room noise.
    const VOICE_LEVEL = 0.08;
    // How long the bar stays open after the last voice. Without it every gap
    // between words would slam it shut.
    const VOICE_HOLD_MS = 700;
    // audio_capture.rs EMIT_INTERVAL. Used to work out how far between two
    // levels we are, so the waveform slides continuously instead of stepping one
    // whole sample every 33ms.
    const LEVEL_INTERVAL_MS = 33;

    // The waveform's own shape, all settled in the prototype (bd slugtale-6w6).
    // The user space the envelope is drawn in; stretched to whatever width the
    // tail ends up, hence preserveAspectRatio="none".
    const VIEW_WIDTH = 160;
    const VIEW_MIDDLE = 13;
    // ~2.4s of history at one sample per level.
    const HISTORY = 72;
    // Attack is deliberately near-instant and only the release is slow: smoothing
    // the rise is what makes a meter feel broken — you speak and nothing happens.
    // At 0.21 the release settles in ~140ms, which is the most lag that still
    // reads as reacting to you rather than describing what you already said.
    const ATTACK = 0.55;
    const RELEASE = 0.21;
    // Half-thickness of the strip when silent. Nothing may move this line.
    const FLOOR = 0.7;
    // How far the two edges breathe, and how fast the undulation travels. The
    // depth is multiplied by the level everywhere it is used, so at silence the
    // wave is exactly flat and exactly still — there is no idle animation here
    // pretending to be speech.
    const FLOW_DEPTH = 0.19;
    const FLOW_RATE = 0.00183;

    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const halo = document.getElementById("halo");
    const label = document.getElementById("label");
    const elapsed = document.getElementById("elapsed");
    const envelope = document.getElementById("envelope");

    // `invoke` is the shared Tauri bridge (tauri-bridge.js), loaded before this
    // page's own script.
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

    // Transcribing holds the bar open for its whole duration: it is the one time
    // the bar has something to report that the user did not ask to see. Voice
    // holds it open too, but only while it lasts — and hover wins over both,
    // because a pointer on the bar is a reach for Stop or Cancel.
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

    // The backend drives the bar's state: "recording" while capturing audio,
    // "transcribing" while the model runs after Stop (slugtale-0t4). The clock
    // restarts with each phase, so it always reads as "how long has this been
    // going" for whatever the bar is currently doing.
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

    // One level per emit, newest at the right of the strip. The follower rises
    // almost instantly and falls slowly, which is what turns syllable-rate spikes
    // into a body of sound rather than a picket fence.
    function setAudioLevel(level) {
      const value = Number.isFinite(level) ? Math.max(0, Math.min(1, level)) : 0;
      targetLevel = value;
      lastLevelAt = Date.now();

      follower += (value - follower) * (value > follower ? ATTACK : RELEASE);
      if (reduceMotion) {
        // Keep one stationary silhouette and change only its amplitude. Shifting
        // the history would move every prior sample left on each event, which is
        // still travel even though frame interpolation is disabled.
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

    // Catmull-Rom as cubic beziers, so the curve passes through every sample
    // instead of cutting the corner off each one.
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

    // Half-thickness for one sample. Older samples taper, so the wave decays into
    // the past instead of being chopped off at the left edge, and the modulation
    // is scaled by the sample's own value so silence is exactly flat.
    function edge(value, age, wobble) {
      return Math.max(FLOOR, value * 11 * (0.55 + 0.45 * age) * (1 + value * wobble));
    }

    function renderWave(now) {
      // Where we are between two levels, which is what makes the wave glide
      // rather than teleport one sample every 33ms.
      const step = reduceMotion
        ? 0
        : Math.max(0, Math.min(1, (now - lastLevelAt) / LEVEL_INTERVAL_MS));
      const spacing = VIEW_WIDTH / (HISTORY - 2);
      const top = [];
      const bottom = [];

      for (let i = 0; i < HISTORY; i += 1) {
        const x = VIEW_WIDTH - ((HISTORY - 1 - i) - step) * spacing;
        const age = i / (HISTORY - 1);
        // Two frequencies, offset phases: the two edges never agree, and a pair
        // that disagrees reads as a body of liquid where a mirrored pair reads as
        // a chart.
        const topWobble = reduceMotion ? 0 : FLOW_DEPTH * Math.sin(age * 5.5 - drift);
        const bottomWobble = reduceMotion ? 0 : FLOW_DEPTH * Math.sin(age * 4.2 - drift * 0.75 + 2.3);
        top.push([x, VIEW_MIDDLE - edge(history[i], age, topWobble)]);
        bottom.push([x, VIEW_MIDDLE + edge(history[i], age, bottomWobble)]);
      }

      envelope.setAttribute("d", splinePath(top) + " " + splinePath(bottom.reverse(), false) + " Z");
    }

    // A transparent window still swallows clicks, and this one is sized for the
    // expanded pill, so while collapsed most of it is invisible surface parked
    // over the user's document. The backend hit-tests the pointer against the
    // part that actually paints and hands the rest back to the app underneath;
    // its answer doubles as the hover state, since a click-through window never
    // sees a mouse event of its own.
    async function pollPointer() {
      const expanded = document.body.dataset.expanded === "true";
      const over = await invoke("dictation_bar_pointer_over", { expanded });
      pointerOver = over === true;
      renderExpansion(expansionReason(Date.now()));
    }

    // The bar is hidden between dictations and the webview keeps running, so the
    // poll is tied to visibility rather than left ticking for the life of the app.
    function setVisible(visible) {
      if (visible && pointerPoll === null) {
        // A fresh dictation starts from silence rather than inheriting the tail
        // of the previous one.
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

      // Time-based rather than per-frame, so the undulation travels at the same
      // speed on a 120Hz display as on a 60Hz one.
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

    // The poll would give the app underneath its clicks back on the next tick
    // anyway, but that leaves up to a tick where the pointer has left the orb and
    // the window is still swallowing input. While the pointer is on the bar the
    // window does receive mouse events, so the moment it leaves is free to catch.
    document.querySelector(".bar").addEventListener("mouseleave", pollPointer);

    // Escape cancels the dictation and discards it (ADR-0014). Once transcription
    // has begun the audio is already captured, so Escape no longer applies.
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

    // The backend drives the bar's phases and appearance; `listen` is the shared
    // bridge's event accessor (tauri-bridge.js).
    const listen = tauriEvent();
    if (listen) {
      listen("dictation-phase", (event) => setPhase(event.payload));
      listen("dictation-appearance", (event) => setAppearance(event.payload));
      listen("dictation-audio-level", (event) => setAudioLevel(event.payload));
      listen("dictation-visibility", (event) => setVisible(event.payload === true));
    }
