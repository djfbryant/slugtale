import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const settingsHtml = readFileSync(new URL("../src/index.html", import.meta.url), "utf8");

// The Usage pane is driven entirely by one command's answer, so the harness only
// has to stand up enough DOM for the script to run and then read back what the
// pane put on screen.
function loadSettingsScript({ invoke }) {
  const [, script] = settingsHtml.match(/<script>\s*([\s\S]*?)\s*<\/script>/);
  const elements = new Map();

  function createElement(tagName, id = "") {
    return {
      id,
      tagName,
      children: [],
      className: "",
      classList: { add() {}, remove() {}, toggle() {} },
      dataset: {},
      checked: false,
      disabled: false,
      hidden: false,
      innerHTML: "",
      style: { removeProperty() {} },
      textContent: "",
      value: "",
      addEventListener() {},
      setAttribute() {},
      focus() {},
      append(...children) {
        this.children.push(...children);
      },
      querySelector() {
        return createElement("div");
      },
      replaceChildren(...children) {
        this.children = children;
      }
    };
  }

  function element(id) {
    if (!elements.has(id)) elements.set(id, createElement("div", id));
    return elements.get(id);
  }

  const context = {
    console,
    navigator: { userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" },
    document: {
      createElement,
      getElementById: element,
      // Nothing in these tests types into the estimate field, so no element is
      // ever the active one — which is what lets renderUsage refill it.
      activeElement: null,
      addEventListener() {}
    },
    setTimeout(callback) {
      return callback && 0;
    },
    window: {
      __TAURI__: { core: { invoke } },
      addEventListener() {}
    }
  };
  context.globalThis = context;

  const testableScript = script.replace(
    /\s*init\(\);\s*$/,
    "\nwindow.__slugtaleTest = { loadUsage, setUsageStoring, saveTypingEstimate, askRedoTypingChallenges, acceptUsageConfirm, cancelUsageConfirm };\n"
  );
  vm.runInNewContext(testableScript, context);

  return { elements, ...context.window.__slugtaleTest };
}

function summary(overrides = {}) {
  return {
    store_usage: true,
    today: { dictations: 2, words: 240, time_saved: "About 4 min" },
    this_week: { dictations: 9, words: 1100, time_saved: "About 18 min" },
    all_time: { dictations: 40, words: 5200, time_saved: "About 1 hr 25 min" },
    measured_wpm: 62,
    typed_estimate: null,
    completed_challenges: 3,
    challenge_count: 3,
    ...overrides
  };
}

test("the settings rail puts Usage between Dictation and Model", () => {
  const railOrder = [...settingsHtml.matchAll(/data-pane="([a-z-]+)"/g)].map((match) => match[1]);
  assert.deepEqual(railOrder, ["status", "dictation", "usage", "model", "general"]);
});

test("Usage shows time saved as the hero and counts under it", async () => {
  const { elements, loadUsage } = loadSettingsScript({
    async invoke() {
      return summary();
    }
  });

  await loadUsage();

  assert.equal(elements.get("usage-hero-value").textContent, "About 1 hr 25 min");
  assert.equal(elements.get("usage-today-saved").textContent, "About 4 min");
  assert.equal(elements.get("usage-today-counts").textContent, "2 dictations · 240 words");
  assert.equal(elements.get("usage-week-counts").textContent, "9 dictations · 1100 words");
  assert.equal(elements.get("usage-all-counts").textContent, "40 dictations · 5200 words");
});

test("Usage never shows speaking duration as its own number", () => {
  // Speaking duration is stored and is half of the Time Saved sum, but it is
  // not a number the pane reports (ADR-0025).
  const usagePane = settingsHtml.match(/<section id="pane-usage"[\s\S]*?<\/section>/)[0];
  assert.doesNotMatch(usagePane, /speaking time as/i);
  assert.doesNotMatch(usagePane, /speaking_seconds/);
  // The summary the pane renders from carries no duration field at all, so it
  // cannot show one by accident.
  assert.doesNotMatch(settingsHtml, /usage\.\w+\.speaking/);
});

test("with storing off the pane explains that nothing is stored and hides the zeroed spans", async () => {
  const { elements, loadUsage } = loadSettingsScript({
    async invoke() {
      return summary({
        store_usage: false,
        today: { dictations: 0, words: 0, time_saved: "0 min" },
        this_week: { dictations: 0, words: 0, time_saved: "0 min" },
        all_time: { dictations: 0, words: 0, time_saved: "0 min" }
      });
    }
  });

  await loadUsage();

  assert.equal(elements.get("usage-store-toggle").checked, false);
  assert.equal(elements.get("usage-empty").hidden, false);
  assert.match(elements.get("usage-empty").textContent, /Nothing is being stored/);
  // Still offer the baseline: the Typing Challenges work with the toggle off.
  assert.match(elements.get("usage-empty").textContent, /typing speed/);
  assert.equal(elements.get("usage-spans").hidden, true);
});

test("with storing on but nothing counted yet the pane says so", async () => {
  const { elements, loadUsage } = loadSettingsScript({
    async invoke() {
      return summary({
        today: { dictations: 0, words: 0, time_saved: "0 min" },
        this_week: { dictations: 0, words: 0, time_saved: "0 min" },
        all_time: { dictations: 0, words: 0, time_saved: "0 min" }
      });
    }
  });

  await loadUsage();

  assert.equal(elements.get("usage-empty").hidden, false);
  assert.equal(elements.get("usage-empty").textContent, "No dictations counted yet.");
  assert.equal(elements.get("usage-spans").hidden, false);
});

test("counts show without a typing baseline and time saved stays a hole", async () => {
  const { elements, loadUsage } = loadSettingsScript({
    async invoke() {
      return summary({
        measured_wpm: null,
        typed_estimate: null,
        completed_challenges: 0,
        today: { dictations: 2, words: 240, time_saved: null },
        this_week: { dictations: 9, words: 1100, time_saved: null },
        all_time: { dictations: 40, words: 5200, time_saved: null }
      });
    }
  });

  await loadUsage();

  assert.equal(elements.get("usage-hero-value").textContent, "—");
  assert.equal(elements.get("usage-hero").dataset.baseline, "false");
  // The counts are real and are shown; only Time Saved is missing.
  assert.equal(elements.get("usage-all-counts").textContent, "40 dictations · 5200 words");
  assert.equal(elements.get("usage-today-saved").textContent, "—");
  // And there is a way out of the hole.
  assert.equal(elements.get("usage-hero-action").hidden, false);
  assert.equal(elements.get("usage-baseline-button").textContent, "Measure my typing speed");
});

test("partway through the challenges the action offers to continue them", async () => {
  const { elements, loadUsage } = loadSettingsScript({
    async invoke() {
      return summary({ measured_wpm: null, completed_challenges: 2 });
    }
  });

  await loadUsage();

  assert.equal(elements.get("usage-baseline-button").textContent, "Continue typing challenge (2 of 3)");
  assert.equal(elements.get("usage-baseline-state").textContent, "2 of 3 typing challenges done.");
});

test("a measured baseline replaces the estimate field rather than letting it be typed over", async () => {
  const { elements, loadUsage } = loadSettingsScript({
    async invoke() {
      return summary({ typed_estimate: 45 });
    }
  });

  await loadUsage();

  assert.match(elements.get("usage-baseline-state").textContent, /62 words per minute, measured/);
  assert.equal(elements.get("usage-estimate-row").hidden, true);
  assert.equal(elements.get("usage-estimate-input").disabled, true);
  assert.equal(elements.get("usage-estimate-save").disabled, true);
  // Measured means the take-the-baseline action is done; Redo is the way back.
  assert.equal(elements.get("usage-hero-action").hidden, true);
  assert.equal(elements.get("usage-redo-button").hidden, false);
});

test("an estimate alone is offered as a stand-in and can be cleared", async () => {
  const { elements, loadUsage } = loadSettingsScript({
    async invoke() {
      return summary({ measured_wpm: null, typed_estimate: 45, completed_challenges: 0 });
    }
  });

  await loadUsage();

  assert.equal(elements.get("usage-estimate-input").value, "45");
  assert.equal(elements.get("usage-estimate-input").disabled, false);
  assert.equal(elements.get("usage-estimate-clear").disabled, false);
  assert.match(elements.get("usage-baseline-state").textContent, /45 words per minute, your estimate/);
});

test("turning storing off asks in the pane before deleting the counts", async () => {
  // The webview implements no JS confirm dialog, so a window.confirm here would
  // silently answer no and the user could never opt out.
  const commands = [];
  const { elements, loadUsage, setUsageStoring } = loadSettingsScript({
    async invoke(command, args) {
      commands.push([command, args]);
      return summary();
    }
  });

  await loadUsage();
  commands.length = 0;
  setUsageStoring(false);

  assert.equal(elements.get("usage-confirm").hidden, false);
  assert.match(elements.get("usage-confirm-text").textContent, /delete/i);
  // And it says what survives, because the challenges were the user's time.
  assert.match(elements.get("usage-confirm-text").textContent, /typing speed is kept/);
  assert.equal(elements.get("usage-confirm-accept").textContent, "Delete counts");
  // Nothing is written until the question is answered, and the switch still
  // shows what is actually stored.
  assert.equal(commands.length, 0);
  assert.equal(elements.get("usage-store-toggle").checked, true);
});

test("confirming the deletion is what finally writes it", async () => {
  const commands = [];
  const { elements, loadUsage, setUsageStoring, acceptUsageConfirm } = loadSettingsScript({
    async invoke(command, args) {
      commands.push([command, args]);
      return summary({ store_usage: false });
    }
  });

  await loadUsage();
  commands.length = 0;
  setUsageStoring(false);
  await acceptUsageConfirm();

  assert.equal(commands.length, 1);
  assert.equal(commands[0][0], "set_usage_storing");
  assert.equal(commands[0][1].enabled, false);
  assert.equal(elements.get("usage-confirm").hidden, true);
});

test("cancelling leaves storing on and writes nothing", async () => {
  const commands = [];
  const { elements, loadUsage, setUsageStoring, cancelUsageConfirm } = loadSettingsScript({
    async invoke(command, args) {
      commands.push([command, args]);
      return summary();
    }
  });

  await loadUsage();
  commands.length = 0;
  setUsageStoring(false);
  cancelUsageConfirm();

  assert.equal(commands.length, 0);
  assert.equal(elements.get("usage-confirm").hidden, true);
  assert.equal(elements.get("usage-store-toggle").checked, true);
});

test("turning storing on does not ask, because it only starts a count", async () => {
  const commands = [];
  const { elements, loadUsage, setUsageStoring } = loadSettingsScript({
    async invoke(command, args) {
      commands.push([command, args]);
      return summary();
    }
  });

  await loadUsage();
  commands.length = 0;
  setUsageStoring(true);
  await Promise.resolve();
  await Promise.resolve();

  assert.equal(elements.get("usage-confirm").hidden, true);
  assert.equal(commands.length, 1);
  assert.equal(commands[0][0], "set_usage_storing");
  assert.equal(commands[0][1].enabled, true);
});

test("a refused estimate leaves the pane showing what is actually stored", async () => {
  const { elements, loadUsage, saveTypingEstimate } = loadSettingsScript({
    async invoke(command) {
      if (command === "set_typing_estimate") {
        throw new Error("a typing estimate must be between 10 and 150 words per minute, not 400");
      }
      return summary({ measured_wpm: null, typed_estimate: 45, completed_challenges: 0 });
    }
  });

  await loadUsage();
  elements.get("usage-estimate-input").value = "400";
  saveTypingEstimate();
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();

  assert.match(elements.get("usage-message").textContent, /between 10 and 150/);
  assert.equal(elements.get("usage-estimate-input").value, "45");
});

test("redoing the challenges warns that the time saved already on screen will move", async () => {
  const commands = [];
  const { elements, loadUsage, askRedoTypingChallenges, acceptUsageConfirm } = loadSettingsScript({
    async invoke(command) {
      commands.push(command);
      return summary({ measured_wpm: null, completed_challenges: 0 });
    }
  });

  await loadUsage();
  commands.length = 0;
  askRedoTypingChallenges();

  assert.equal(elements.get("usage-confirm").hidden, false);
  assert.match(elements.get("usage-confirm-text").textContent, /replaces your measured speed/);
  assert.match(elements.get("usage-confirm-text").textContent, /will move/);
  assert.equal(commands.length, 0);

  await acceptUsageConfirm();

  // Redoing drops the old results and opens the window on the first passage,
  // rather than leaving the user to find their way back in.
  assert.deepEqual(commands, [
    "redo_typing_challenges",
    "get_usage_summary",
    "open_typing_challenge"
  ]);
});

test("the pane never relies on a browser dialog to confirm a deletion", () => {
  // This webview implements no JS confirm/alert panel: a window.confirm would
  // answer no every time, and the destructive actions would be unreachable.
  assert.doesNotMatch(settingsHtml, /window\.confirm\s*\(/);
  assert.doesNotMatch(settingsHtml, /window\.alert\s*\(/);
});
