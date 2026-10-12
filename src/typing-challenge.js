
    const fallbackState = {
      passage: "The harbour was quiet that morning, and the boats leaned together at their moorings as if sharing a long and complicated secret.",
      passage_index: 0,
      completed: 0,
      total: 3,
      seconds: 30,
      measured_wpm: null
    };

    let state = fallbackState;
    let words = [];
    let startedAt = null;
    let ticker = null;
    let finished = false;
    let submitting = false;

    function el(id) {
      return document.getElementById(id);
    }

    function setMessage(text, isError = false) {
      const message = el("message");
      message.classList.toggle("error", Boolean(isError));
      message.textContent = text || "";
    }

    function renderPassage() {
      words = (state.passage || "").split(/\s+/).filter(Boolean);
      el("passage").innerHTML = words
        .map((word, index) => `<span class="word" data-index="${index}"></span>`)
        .join(" ");
      words.forEach((word, index) => {
        el("passage").querySelector(`[data-index="${index}"]`).textContent = word;
      });
      markTypedWords("");
    }

    function markTypedWords(typed) {
      const typedWords = typed.split(/\s+/).filter(Boolean);
      const inWord = typed.length > 0 && !/\s$/.test(typed);
      const currentIndex = inWord ? typedWords.length - 1 : typedWords.length;

      words.forEach((word, index) => {
        const span = el("passage").querySelector(`[data-index="${index}"]`);
        if (!span) return;
        if (index < typedWords.length && !(inWord && index === currentIndex)) {
          span.dataset.state = typedWords[index] === word ? "correct" : "wrong";
        } else if (index === currentIndex) {
          span.dataset.state = "current";
        } else {
          delete span.dataset.state;
        }
      });
    }

    function renderState() {
      const measured = state.measured_wpm !== null && state.measured_wpm !== undefined;
      const allDone = state.passage_index === null || state.passage_index === undefined;

      el("progress").textContent = allDone
        ? `${state.total} of ${state.total} challenges done`
        : `Challenge ${state.completed + 1} of ${state.total}`;
      el("clock").textContent = String(state.seconds);
      el("clock").dataset.running = "false";

      if (allDone) {
        el("run").hidden = true;
        el("typing").hidden = true;
        el("abort").hidden = true;
        el("next").hidden = true;
        el("done").hidden = false;
        el("done-value").textContent = measured ? `${state.measured_wpm} WPM` : "—";
        el("done-note").textContent = measured
          ? "This is the median of your three runs. Time saved in Settings now uses it."
          : "";
        el("subtitle").textContent = "All three challenges are done.";
        el("close").textContent = "Done";
        return;
      }

      el("run").hidden = false;
      el("typing").hidden = false;
      el("done").hidden = true;
      el("close").textContent = "Close";
      el("subtitle").textContent =
        `Type the passage as accurately as you can. Corrections are fine. The ${state.seconds} seconds start when you do.`;
      renderPassage();
      resetRun();
    }

    function resetRun() {
      stopTicker();
      startedAt = null;
      finished = false;
      el("typing").value = "";
      el("typing").disabled = false;
      el("clock").textContent = String(state.seconds);
      el("clock").dataset.running = "false";
      el("abort").hidden = true;
      el("next").hidden = true;
      markTypedWords("");
      setMessage("");
    }

    function stopTicker() {
      if (ticker !== null) {
        clearInterval(ticker);
        ticker = null;
      }
    }

    function remainingSeconds() {
      if (startedAt === null) return state.seconds;
      const elapsed = (Date.now() - startedAt) / 1000;
      return Math.max(0, Math.ceil(state.seconds - elapsed));
    }

    function startRun() {
      startedAt = Date.now();
      el("clock").dataset.running = "true";
      el("abort").hidden = false;
      ticker = setInterval(() => {
        const left = remainingSeconds();
        el("clock").textContent = String(left);
        if (left <= 0) finishRun();
      }, 100);
    }

    async function finishRun() {
      if (finished || submitting) return;
      finished = true;
      stopTicker();

      const typed = el("typing").value;
      el("typing").disabled = true;
      el("clock").textContent = "0";
      el("clock").dataset.running = "false";
      el("abort").hidden = false;

      const invoke = tauriInvoke();
      if (!invoke) {
        setMessage("Typing challenges only score inside the Slugtale app.");
        return;
      }

      submitting = true;
      setMessage("Scoring…");
      try {
        state = await invoke("submit_typing_challenge", {
          passageIndex: state.passage_index,
          typed
        });
        submitting = false;
        if (state.passage_index === null || state.passage_index === undefined) {
          renderState();
          return;
        }
        el("next").hidden = false;
        el("next").focus();
        setMessage(`Challenge ${state.completed} of ${state.total} done.`);
      } catch (error) {
        submitting = false;
        finished = false;
        el("typing").disabled = false;
        setMessage(String(error), true);
      }
    }

    async function loadState() {
      const invoke = tauriInvoke();
      if (!invoke) {
        renderState();
        return;
      }

      try {
        state = await invoke("get_typing_challenge");
        renderState();
      } catch (error) {
        setMessage(String(error), true);
      }
    }

    function closeWindow() {
      const invoke = tauriInvoke();
      if (invoke) {
        invoke("close_typing_challenge").catch(() => window.close());
        return;
      }
      window.close();
    }

    function init() {
      el("typing").addEventListener("input", (event) => {
        if (finished) return;
        if (startedAt === null && event.target.value.length > 0) startRun();
        markTypedWords(event.target.value);
        if (startedAt !== null && remainingSeconds() <= 0) finishRun();
      });

      el("typing").addEventListener("keydown", (event) => {
        if (event.key === "Enter") event.preventDefault();
      });

      el("abort").addEventListener("click", () => {
        loadState();
      });
      el("next").addEventListener("click", () => renderState());
      el("close").addEventListener("click", closeWindow);

      el("typing").focus();
      loadState();
    }

    init();
