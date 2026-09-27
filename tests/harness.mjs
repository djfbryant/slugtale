// One harness for the three pages Slugtale ships.
//
// Each page is a plain HTML file with an inline `<script>`, and each one was
// loaded by a hand-rolled copy of this file: extract the first `<script>` with a
// regex, rewrite the trailing `init();` into an export block, and stand up a DOM
// invented to fit that one test file. Five copies meant the harness interface was
// the shape of the page's source text rather than the shape of the page, so a
// second `<script>` block would have been silently dropped while the tests stayed
// green.
//
// `runPage` reads every inline script block, removes the named bootstrap calls
// rather than assuming where they sit, and appends one export block. It throws
// rather than degrading when a page stops matching: a script block with a `src`,
// or an export the page does not define.

import { readFileSync } from "node:fs";
import vm from "node:vm";

const INLINE_SCRIPT = /<script(?<attrs>[^>]*)>(?<body>[\s\S]*?)<\/script>/g;

function readPage(file, markup) {
  const html = markup ?? readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8");
  const scripts = [];
  for (const match of html.matchAll(INLINE_SCRIPT)) {
    if (/\bsrc\s*=/.test(match.groups.attrs)) {
      throw new Error(
        `${file} loads a script by src, which this harness does not follow. ` +
          `Add its body to the page, or teach runPage to read it.`,
      );
    }
    scripts.push(match.groups.body);
  }
  if (scripts.length === 0) throw new Error(`${file} has no inline script to run`);
  return { html, script: scripts.join("\n") };
}

const TAG = /<(\/)?([a-zA-Z][\w-]*)((?:\s+[^\s=>]+(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+))?)*)\s*(\/)?>/g;
const ATTRIBUTE = /([^\s=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g;

function readAttributes(source) {
  const attributes = new Map();
  for (const match of (source || "").matchAll(ATTRIBUTE)) {
    attributes.set(match[1], match[2] ?? match[3] ?? match[4] ?? "");
  }
  return attributes;
}

// Pages assign `innerHTML` and then query into it — the Typing Challenge paints
// its passage as one span per word and marks each by `data-index`. Parsing the
// markup on assignment is what makes a real page work against this harness,
// instead of each test hand-parsing the selectors its own page happens to use.
function parseFragment(html, host) {
  const stack = [host];
  let cursor = 0;

  const textSince = (index) => html.slice(cursor, index);

  for (const match of html.matchAll(TAG)) {
    const top = stack[stack.length - 1];
    const text = textSince(match.index);
    if (text.trim()) top.textContent = (top.textContent || "") + text;

    if (match[1]) {
      if (stack.length > 1) stack.pop();
      cursor = match.index + match[0].length;
      continue;
    }
    cursor = match.index + match[0].length;

    const child = createNode(match[2]);
    for (const [name, value] of readAttributes(match[3])) {
      if (name === "class") {
        value.split(/\s+/).filter(Boolean).forEach((entry) => child.classes.add(entry));
        child.className = value;
      } else if (name.startsWith("data-")) {
        child.dataset[camelCase(name.slice(5))] = value;
      } else {
        child.attributes.set(name, value);
      }
    }
    top.children.push(child);
    if (!match[4]) stack.push(child);
  }

  const tail = textSince(cursor);
  const top = stack[stack.length - 1];
  if (tail.trim()) top.textContent = (top.textContent || "") + tail;
  return host.children;
}

function camelCase(name) {
  return name.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
}

// A node is deliberately generous: every page reaches for a different slice of
// the DOM, and the point of sharing one implementation is that a page does not
// have to be the reason the harness grows a new one.
function createNode(tagName = "div", id = "") {
  const attributes = new Map();
  const classes = new Set();
  const listeners = new Map();
  const node = {
    id,
    tagName,
    dataset: {},
    children: [],
    attributes,
    listeners,
    classes,
    checked: false,
    className: "",
    disabled: false,
    hidden: false,
    textContent: "",
    value: "",
    style: {
      properties: new Map(),
      removeProperty(name) {
        this.properties.delete(name);
      },
      setProperty(name, value) {
        this.properties.set(name, value);
      }
    },
    classList: {
      add(...names) {
        names.forEach((name) => classes.add(name));
      },
      contains(name) {
        return classes.has(name);
      },
      remove(...names) {
        names.forEach((name) => classes.delete(name));
      },
      toggle(name, force) {
        const on = force === undefined ? !classes.has(name) : Boolean(force);
        if (on) classes.add(name);
        else classes.delete(name);
        return on;
      }
    },
    addEventListener(type, handler) {
      if (!listeners.has(type)) listeners.set(type, []);
      listeners.get(type).push(handler);
    },
    click(event = {}) {
      node.dispatch("click", event);
    },
    append(...nodes) {
      node.children.push(...nodes);
    },
    dispatch(type, event = {}) {
      const handlers = listeners.get(type) || [];
      const payload = { preventDefault() {}, stopPropagation() {}, target: node, ...event };
      handlers.forEach((handler) => handler(payload));
    },
    focus() {},
    getAttribute(name) {
      return attributes.has(name) ? attributes.get(name) : null;
    },
    matches(selector) {
      const byId = selector.match(/^#(.+)$/);
      if (byId) return node.id === byId[1];
      const byClass = selector.match(/^\.([\w-]+)$/);
      if (byClass) return classes.has(byClass[1]);
      const byData = selector.match(/^\[data-([\w-]+)="([^"]*)"\]$/);
      if (byData) return String(node.dataset[byData[1]]) === byData[2];
      return false;
    },
    querySelector(selector) {
      return node.querySelectorAll(selector)[0] || null;
    },
    querySelectorAll(selector) {
      const found = [];
      const visit = (parent) => {
        parent.children.forEach((child) => {
          if (child && typeof child.matches === "function" && child.matches(selector)) found.push(child);
          if (child && Array.isArray(child.children)) visit(child);
        });
      };
      visit(node);
      return found;
    },
    remove() {
      node.children.length = 0;
    },
    replaceChildren(...nodes) {
      node.children = nodes;
    },
    setAttribute(name, value) {
      attributes.set(name, value);
    }
  };

  let markup = "";
  Object.defineProperty(node, "innerHTML", {
    get: () => markup,
    set(value) {
      markup = String(value);
      node.children = [];
      parseFragment(markup, node);
    }
  });

  return node;
}

function createDocument({ root } = {}) {
  const nodes = new Map();
  const documentListeners = new Map();

  function element(id) {
    if (!nodes.has(id)) nodes.set(id, createNode("div", id));
    return nodes.get(id);
  }

  const document = {
    activeElement: null,
    body: createNode("body"),
    createElement: (tagName) => createNode(tagName),
    documentElement: {
      style: {
        setProperty(name, value) {
          root.style.setProperty(name, value);
        }
      }
    },
    getElementById: element,
    addEventListener(type, handler) {
      if (!documentListeners.has(type)) documentListeners.set(type, []);
      documentListeners.get(type).push(handler);
    },
    dispatch(type, event = {}) {
      (documentListeners.get(type) || []).forEach((handler) =>
        handler({ preventDefault() {}, ...event }),
      );
    },
    // The Dictation Bar reaches the shell by class, the Typing Challenge by id.
    querySelector: element,
    querySelectorAll: (selector) => (root.matches(selector) ? [root] : []),
    nodes
  };
  return document;
}

// `manual` timers are for the pages whose behaviour is a countdown: the test
// advances them rather than waiting out the real seconds.
function createTimers(mode) {
  const timeouts = [];
  const intervals = [];
  const frames = [];

  if (mode === "manual") {
    return {
      clearInterval() {},
      context: {
        clearInterval() {},
        requestAnimationFrame(callback) {
          frames.push(callback);
          return frames.length;
        },
        setInterval(callback, delay) {
          intervals.push({ callback, delay });
          return intervals.length;
        },
        setTimeout(callback) {
          timeouts.push(callback);
          return timeouts.length;
        }
      },
      frames,
      intervals,
      timeouts
    };
  }

  return {
    clearInterval() {},
    // Run immediately: a test that needs ordering should await, not race a clock.
    context: {
      clearInterval() {},
      requestAnimationFrame(callback) {
        frames.push(callback);
        return frames.length;
      },
      setInterval(callback, delay) {
        intervals.push({ callback });
        return intervals.length;
      },
      setTimeout(callback) {
        timeouts.push(callback);
        return typeof callback === "function" ? (callback(), 0) : 0;
      }
    },
    frames,
    intervals,
    timeouts
  };
}

/**
 * Run a page's script against a fake DOM and hand back the named handles.
 *
 * @param {string} file        page name under `src/`, e.g. `index.html`
 * @param {object} options
 * @param {string} options.markup  page source to run instead of reading `src/`
 * @param {Function} options.invoke   stands in for the Tauri bridge
 * @param {string[]} options.exports  top-level names the page must define
 * @param {string} options.exportSource  raw export block, for a handle that is a closure over private state
 * @param {string[]} options.bootstrap top-level calls to strip, default `["init"]`
 * @param {boolean} options.runBootstrap keep the bootstrap call instead of stripping it
 * @param {string} options.userAgent
 * @param {number} options.now        fixed `Date.now()` for the countdown pages
 * @param {"immediate"|"manual"} options.timers
 * @param {string} options.exportName where the handles are published
 */
export function runPage(file, options = {}) {
  const {
    bootstrap = ["init"],
    exportName = "__slugtaleTest",
    exports: exportNames = [],
    invoke = async () => undefined,
    now,
    reduceMotion = false,
    runBootstrap = false,
    timers: timerMode = "immediate",
    userAgent = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)"
  } = options;

  const page = readPage(file, options.markup);
  const root = createNode("html", "root");
  const document = createDocument({ root });
  const timers = createTimers(timerMode);

  // Strip the bootstrap calls wherever they sit, so a page can start itself from
  // a second script block without the harness deciding it runs.
  let source = page.script;
  bootstrap.forEach((name) => {
    source = source.replace(new RegExp(`^\\s*${name}\\(\\);?\\s*$`, "gm"), "");
  });

  if (exportNames.length > 0) {
    source += `\nwindow.${exportName} = { ${exportNames.join(", ")} };`;
  } else if (options.exportSource) {
    source += `\nwindow.${exportName} = { ${options.exportSource} };`;
  }

  const window = {
    __TAURI__: {
      core: { invoke },
      event: { listen() {} }
    },
    addEventListener() {},
    close() {},
    document,
    matchMedia: () => ({ matches: reduceMotion, addEventListener() {} })
  };

  // Only the globals a page cannot derive are injected. Re-passing `Promise`,
  // `Math` and friends from the host would mix two realms' microtask queues into
  // a test that counts microtasks, and the counts are how these tests sequence
  // themselves.
  const context = {
    clearInterval: timers.context.clearInterval,
    console,
    document,
    navigator: { userAgent },
    requestAnimationFrame: timers.context.requestAnimationFrame,
    setInterval: timers.context.setInterval,
    setTimeout: timers.context.setTimeout,
    window
  };
  if (now !== undefined) {
    const read = typeof now === "function" ? now : () => now;
    context.Date = { now: read };
  }
  context.globalThis = context;
  window.globalThis = context;
  window.document = document;

  vm.runInNewContext(source, context, { filename: file });

  if (runBootstrap && bootstrap.length > 0) {
    vm.runInNewContext(bootstrap.map((name) => `${name}();`).join("\n"), context, { filename: file });
  }

  const api = window[exportName] || {};
  const missing = exportNames.filter((name) => !(name in api));
  if (missing.length > 0) {
    throw new Error(
      `${file} does not define ${missing.join(", ")}. The page's top-level names ` +
        `changed, so this test is asserting against nothing.`,
    );
  }

  return {
    api,
    document,
    elements: document.nodes,
    intervals: timers.intervals,
    root,
    rootStyle: root.style.properties,
    timeouts: timers.timeouts,
    // The shapes the pages differ on, so callers do not reach into the harness.
    keydown: (key, event = {}) =>
      document.dispatch("keydown", { key, ...event }),
    flushNextTimeout() {
      const callback = timers.timeouts.shift();
      if (!callback) throw new Error("No pending timer to flush");
      callback();
    }
  };
}

export { createNode, readPage };
