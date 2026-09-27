// One harness for the three pages Slugtale ships.
//
// Each page is a plain HTML file with an inline `<script>`, and each one was
// loaded by a hand-rolled copy of this file: extract the first `<script>` with a
// regex, rewrite the trailing `init();` into an export block, and stand up a DOM
// that invented an element for whatever `getElementById` was asked for. Five
// copies meant the harness interface was the shape of the page's source text
// rather than the shape of the page, and a fake that answers any id makes a test
// pass against a page whose markup the script has since broken.
//
// `runPage` reads the page's own markup and its own scripts. The document is
// built from the ids and the elements the page really ships, so a test that reads
// a node is reading the page's node. It fails loudly rather than degrading: a
// script block with a `src`, a handle the page does not define, a selector the
// fake cannot answer honestly, an id the page does not have.

import { readFileSync } from "node:fs";
import vm from "node:vm";

const SCRIPT = /<script(?<attrs>[^>]*)>(?<body>[\s\S]*?)<\/script>/g;
const TAG = /<(\/)?([a-zA-Z][\w:-]*)((?:\s+[^\s=>]+(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+))?)*)\s*(\/)?>/g;
const ATTRIBUTE = /([^\s=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g;

// HTML's void elements carry no closing tag, and the SVG shapes in the icon
// markup are written the same way. Without this list each one would swallow the
// rest of the fragment as children, and a query that finds them in a browser
// would find nothing here.
const VOID = new Set([
  "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
  "source", "track", "wbr",
  "circle", "ellipse", "line", "path", "polygon", "polyline", "rect", "stop", "use"
]);

const UNSUPPORTED_SELECTOR = /:(?!first-child\b|last-child\b|nth-child\()|\s|>|\+|~/;

function camelCase(name) {
  return name.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
}

function readAttributes(source) {
  const attributes = new Map();
  for (const match of (source || "").matchAll(ATTRIBUTE)) {
    attributes.set(match[1], match[2] ?? match[3] ?? match[4] ?? "");
  }
  return attributes;
}

class FakeDomError extends Error {}

function textRun(value) {
  const text = String(value);
  return { children: [], text, textContent: text };
}

function createNode(tagName, registry) {
  const attributes = new Map();
  const classes = new Set();
  const listeners = new Map();
  let ownText = "";
  let markup = "";

  const node = {
    tagName: tagName.toUpperCase(),
    children: [],
    classes,
    dataset: {},
    attributes,
    checked: false,
    disabled: false,
    hidden: false,
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
      contains: (name) => classes.has(name),
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
    append(...nodes) {
      nodes.forEach((child) => {
        // A page may append a bare string, which a browser turns into a text run.
        const entry = typeof child === "string" || typeof child === "number" ? textRun(child) : child;
        node.children.push(entry);
        if (entry.text === undefined) entry.parentNode = node;
      });
    },
    dispatch(type, event = {}) {
      (listeners.get(type) || []).forEach((handler) =>
        handler({ preventDefault() {}, stopPropagation() {}, target: node, ...event }),
      );
    },
    focus() {
      registry.document.activeElement = node;
    },
    getAttribute: (name) => (attributes.has(name) ? attributes.get(name) : null),
    matches(selector) {
      return selectorMatches(node, selector);
    },
    querySelector(selector) {
      rejectUnsupported(selector);
      return descendants(node).find((entry) => selectorMatches(entry, selector)) || null;
    },
    querySelectorAll(selector) {
      rejectUnsupported(selector);
      return descendants(node).filter((entry) => selectorMatches(entry, selector));
    },
    removeAttribute: (name) => attributes.delete(name),
    get listeners() {
      return listeners;
    },
    get parent() {
      return node.parentNode || null;
    },
    remove() {
      const owner = node.parentNode;
      if (!owner) return;
      owner.children = owner.children.filter((child) => child !== node);
      node.parentNode = null;
    },
    replaceChildren(...nodes) {
      node.children = [];
      node.append(...nodes);
    },
    setAttribute(name, value) {
      attributes.set(name, value);
    }
  };

  node.parentNode = null;
  node.click = (event = {}) => node.dispatch("click", event);

  // `className` and the class set are one value, as in a browser. Keeping them as
  // two stores is how a page that styles a node by class ends up matching nothing
  // here and everything in the real window.
  Object.defineProperty(node, "className", {
    get: () => [...classes].join(" "),
    set(value) {
      classes.clear();
      String(value || "").split(/\s+/).filter(Boolean).forEach((entry) => classes.add(entry));
    }
  });

  // Reading `textContent` is how a test checks what a person would read, so it
  // has to include the text of the children, not just the text set on this node.
  Object.defineProperty(node, "textContent", {
    get: () => ownText + node.children.map((child) => child.textContent || "").join(""),
    set(value) {
      node.children = [];
      ownText = String(value ?? "");
    }
  });

  Object.defineProperty(node, "innerHTML", {
    get: () => markup,
    set(value) {
      markup = String(value ?? "");
      ownText = "";
      node.children = [];
      appendFragment(node, markup, registry);
    }
  });

  return node;
}

// Parsing the markup on assignment is what lets a page query into what it
// painted, instead of each test hand-parsing the selectors its own page uses.
function appendFragment(host, html, registry) {
  const stack = [host];
  let cursor = 0;
  const textBefore = (index) => html.slice(cursor, index);

  for (const match of html.matchAll(TAG)) {
    const top = stack[stack.length - 1];
    const text = textBefore(match.index);
    if (text) top.append(textRun(text));

    cursor = match.index + match[0].length;
    if (match[1]) {
      if (stack.length > 1) stack.pop();
      continue;
    }

    const child = createNode(match[2], registry);
    for (const [name, value] of readAttributes(match[3])) {
      if (name === "class") {
        child.className = value;
      } else if (name === "id") {
        child.id = value;
        registry.byId.set(value, child);
      } else if (name.startsWith("data-")) {
        child.dataset[camelCase(name.slice(5))] = value;
      } else {
        child.attributes.set(name, value);
      }
    }
    top.append(child);
    if (!match[4] && !VOID.has(match[2].toLowerCase())) stack.push(child);
  }

  const tail = textBefore(cursor);
  if (tail) stack[stack.length - 1].append(textRun(tail));
}

function rejectUnsupported(selector) {
  if (UNSUPPORTED_SELECTOR.test(selector)) {
    throw new FakeDomError(
      `This fake DOM cannot answer the selector "${selector}". ` +
        `Teach it the selector rather than let a query quietly match nothing.`,
    );
  }
}

function selectorMatches(node, selector) {
  return selector.split(",").some((part) => compoundMatches(node, part.trim()));
}

function compoundMatches(node, selector) {
  return selector
    .split(/(?=[#.[])/)
    .filter(Boolean)
    .every((part) => {
      if (part.startsWith("#")) return node.id === part.slice(1);
      if (part.startsWith(".")) return node.classes.has(part.slice(1));
      if (part.startsWith("[")) {
        const match = part.match(/^\[([\w:-]+)(?:([~|^$*]?=)"([^"]*)")?\]$/);
        if (!match) throw new FakeDomError(`Unsupported attribute selector "${part}"`);
        const [, name, , value] = match;
        const actual =
          name.startsWith("data-") ? node.dataset[camelCase(name.slice(5))] : node.getAttribute(name);
        if (value === undefined) return actual !== undefined && actual !== null;
        return String(actual) === value;
      }
      return node.tagName === part.toUpperCase();
    });
}

function descendants(node, found = []) {
  node.children.forEach((child) => {
    if (child.text !== undefined) return;
    found.push(child);
    descendants(child, found);
  });
  return found;
}

function readPage(file, markup) {
  const html = markup ?? readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8");

  const scripts = [];
  for (const match of html.matchAll(SCRIPT)) {
    if (/\bsrc\s*=/.test(match.groups.attrs)) {
      throw new Error(
        `${file} loads a script by src, which this harness does not follow. ` +
          `Inline the body, or teach runPage to read it.`,
      );
    }
    scripts.push(match.groups.body);
  }
  if (scripts.length === 0) throw new Error(`${file} has no inline script to run`);

  return { script: scripts.join("\n"), shell: html.replace(SCRIPT, "") };
}

function buildDocument(page) {
  const byId = new Map();
  const documentListeners = new Map();
  const registry = { byId, document: null };
  const root = createNode("html", registry);
  const document = {
    activeElement: null,
    body: createNode("body", registry),
    documentElement: root,
    createElement: (tagName) => createNode(tagName, registry),
    addEventListener(type, handler) {
      if (!documentListeners.has(type)) documentListeners.set(type, []);
      documentListeners.get(type).push(handler);
    },
    dispatch(type, event = {}) {
      (documentListeners.get(type) || []).forEach((handler) =>
        handler({ preventDefault() {}, ...event }),
      );
    },
    getElementById: (id) => byId.get(id) || null,
    querySelector(selector) {
      return document.querySelectorAll(selector)[0] || null;
    },
    querySelectorAll(selector) {
      rejectUnsupported(selector);
      return [...descendants(root), ...descendants(document.body)].filter((node) =>
        selectorMatches(node, selector),
      );
    },
    documentListeners
  };
  registry.document = document;

  // The page's own markup, so `getElementById` answers with the element the page
  // ships and a misspelled id is a null the test will notice.
  appendFragment(document.body, page.shell, registry);

  return { byId, document, root };
}

function fixedDate(read) {
  const Shim = function DateShim(...args) {
    return args.length === 0 ? new Date(read()) : new Date(...args);
  };
  Shim.now = read;
  Shim.parse = Date.parse;
  Shim.UTC = Date.UTC;
  return Shim;
}

/**
 * Run a page against a fake DOM built from the page's own markup.
 *
 * @param {string} file            page name under `src/`, e.g. `index.html`
 * @param {object} [options]
 * @param {string} [options.markup]  page source to run instead of reading `src/`
 * @param {Function} [options.invoke]  stands in for the Tauri bridge
 * @param {string[]} [options.exports]  handles the test will read, and that the page must define
 * @param {string} [options.exportSource]  raw export block, for a handle that closes over private state
 * @param {string[]} [options.bootstrap]  top-level calls to strip, default `["init"]`
 * @param {boolean} [options.runBootstrap]  keep the bootstrap call instead of stripping it
 * @param {string} [options.userAgent]
 * @param {number|Function} [options.now]  what the page's `Date.now()` returns
 * @param {boolean} [options.reduceMotion]  what `matchMedia` reports
 * @param {string} [options.exportName]  where the handles are published
 * @returns {{api: object, document: object, elements: Map, intervals: Array,
 *   timeouts: Array, rootStyle: Map, flushNextTimeout: Function, keydown: Function}}
 */
export function runPage(file, options = {}) {
  const {
    bootstrap = ["init"],
    exportName = "__slugtaleTest",
    exports: exportNames = [],
    invoke = async () => undefined,
    now,
    reduceMotion = false,
    runBootstrap = false
  } = options;

  const page = readPage(file, options.markup);
  const { byId, document, root } = buildDocument(page);

  // Timers are always queued and never fired on their own. A test that needs one
  // to happen asks for it, so a page cannot pass a test by waiting and fail for
  // a real user, or the other way round.
  const timeouts = [];
  const intervals = [];
  const timers = {
    clearInterval(handle) {
      if (handle >= 1 && handle <= intervals.length) intervals.splice(handle - 1, 1);
    },
    setInterval(callback, delay) {
      intervals.push({ callback, delay });
      return intervals.length;
    },
    setTimeout(callback, delay) {
      timeouts.push({ callback, delay });
      return timeouts.length;
    }
  };

  // Strip the bootstrap calls wherever they sit, so a page can start itself from
  // a second script block without the harness deciding it runs.
  let source = page.script;
  bootstrap.forEach((name) => {
    source = source.replace(new RegExp(`^\\s*${name}\\(\\);?\\s*$`, "gm"), "");
  });
  if (exportNames.length > 0) {
    const body = options.exportSource || exportNames.join(", ");
    source += `\nwindow.${exportName} = { ${body} };`;
  }

  const window = {
    __TAURI__: { core: { invoke }, event: { listen() {} } },
    addEventListener() {},
    close() {},
    document,
    matchMedia: () => ({ addEventListener() {}, matches: reduceMotion })
  };

  // Only the globals a page cannot derive are injected. Re-passing `Promise`,
  // `Math` and friends from the host would mix two realms' microtask queues into
  // a test that counts microtasks, and the counts are how these tests sequence
  // themselves.
  const context = {
    clearInterval: timers.clearInterval,
    console,
    document,
    navigator: { userAgent: options.userAgent || "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" },
    // A page's animation loop is driven by the test through the page's own
    // render call, so a frame is never scheduled behind a test's back.
    requestAnimationFrame: () => 0,
    setInterval: timers.setInterval,
    setTimeout: timers.setTimeout,
    window
  };
  if (now !== undefined) context.Date = fixedDate(typeof now === "function" ? now : () => now);
  context.globalThis = context;
  window.globalThis = context;

  vm.runInNewContext(source, context, { filename: file });
  if (runBootstrap && bootstrap.length > 0) {
    bootstrap.forEach((name) => context[name]());
  }

  const api = window[exportName] || {};
  const missing = exportNames.filter((name) => !(name in api));
  if (missing.length > 0) {
    throw new Error(
      `${file} exposes no ${missing.join(", ")}. The page's top-level names changed, ` +
        `so this test would be asserting against nothing.`,
    );
  }

  return {
    api,
    document,
    elements: byId,
    intervals,
    rootStyle: root.style.properties,
    timeouts,
    // A page queues its next timer from inside a promise callback, so waiting for
    // one to appear is part of flushing one.
    async flushNextTimeout() {
      for (let spin = 0; timeouts.length === 0 && spin < 10; spin += 1) {
        await Promise.resolve();
      }
      const entry = timeouts.shift();
      if (!entry) throw new Error("No pending timer to flush");
      entry.callback();
    },
    keydown(key, event = {}) {
      document.dispatch("keydown", { key, ...event });
    }
  };
}
