/**
 * Every file a page ships: its markup, its stylesheets and its scripts.
 *
 * The shipped pages keep their JavaScript and CSS in files rather than inline, so
 * a test that greps the `.html` for behaviour it ships would find nothing — and
 * would keep passing if the page stopped shipping that behaviour at all. This
 * reads what a browser would load.
 *
 * @param {string} file page name under `src/`
 * @returns {{markup: string, styles: string, scripts: string, all: string}}
 */
export function shippedSource(file) {
  const html = readFileSync(new URL(file, SRC), "utf8");
  const styles = stylesheetOf(file);
  const scripts = [
    ...html.matchAll(/<script[^>]*\bsrc\s*=\s*"([^"]*)"[^>]*>/g),
  ]
    .map(([, href]) => href)
    .filter((href) => LOCAL_SCRIPT.test(href))
    .map((href) => readFileSync(new URL(href, SRC), "utf8"));
  return {
    markup: html,
    styles,
    scripts: scripts.join("\n"),
    all: `${html}\n${styles}\n${scripts.join("\n")}`
  };
}

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
// `runPage` runs the page's own scripts against the page's own markup. It fails
// loudly rather than degrading: a script block with a `src`, a handle the page
// does not expose, a selector the fake cannot answer honestly.

import { readFileSync } from "node:fs";
import vm from "node:vm";

const SRC = new URL("../src/", import.meta.url);
const SCRIPT = /<script(?<attrs>[^>]*)>(?<body>[\s\S]*?)<\/script>/g;
const TAG = /<(\/)?([a-zA-Z][\w:-]*)((?:\s+[^\s=>]+(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+))?)*)\s*(\/)?>/g;
const ATTRIBUTE = /([^\s=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g;

// HTML's void elements carry no closing tag. Without this list each one opens a
// container, so the elements after it become its children and a query on the host
// no longer finds them where a browser would.
const VOID = new Set([
  "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
  "source", "track", "wbr"
]);

// Only what this matcher can answer honestly. Anything else is an error rather
// than a query that quietly finds nothing.
const SUPPORTED_SELECTOR = /^(\*|[a-zA-Z][\w-]*)?((?:[#.][\w-]+|\[[\w:-]+(?:[~|^$*]?="[^"]*")?\])*)$/;
const BOOL_PROPERTIES = ["checked", "disabled", "hidden", "selected", "readOnly"];
const STYLE_PROPERTY = /^--[\w-]+$|^[a-zA-Z][\w]*$/;

class FakeDomError extends Error {}

function camelCase(name) {
  return name.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
}

function dashCase(name) {
  return name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);
}

function readAttributes(source) {
  const attributes = new Map();
  for (const match of (source || "").matchAll(ATTRIBUTE)) {
    attributes.set(match[1], match[2] ?? match[3] ?? match[4] ?? "");
  }
  return attributes;
}

// A text run is a child, not a node: it answers `textContent` and nothing else.
function textRun(value) {
  const text = String(value);
  return { children: [], text, textContent: text };
}

// `dataset` is one value per node, and a browser stores a string in every one, so
// `el.dataset.state = 5` reads back `"5"`.
function createDataset() {
  return new Proxy(
    {},
    {
      get: (store, key) => (typeof key === "string" ? store[camelCase(key)] : store[key]),
      set: (store, key, value) => {
        store[camelCase(key)] = String(value);
        return true;
      },
      deleteProperty: (store, key) => {
        delete store[camelCase(key)];
        return true;
      },
      has: (store, key) => camelCase(key) in store,
      ownKeys: (store) => Object.keys(store),
      getOwnPropertyDescriptor: () => ({ configurable: true, enumerable: true, value: "" })
    },
  );
}

// One attribute map, and the properties a browser reflects out of it. Two stores
// is how a node ends up styled one way and queried another, and a page that sets
// `bar.style.width` then calls `removeProperty("width")` has to see both.
function createStyle(properties) {
  const target = {
    getPropertyValue: (name) => (properties.has(name) ? properties.get(name) : ""),
    removeProperty(name) {
      const previous = properties.has(name) ? properties.get(name) : "";
      properties.delete(name);
      return previous;
    },
    setProperty(name, value) {
      properties.set(name, String(value));
    }
  };
  return new Proxy(target, {
    get: (base, name) => {
      if (name in base) return base[name];
      return typeof name === "string" ? properties.get(name) || "" : undefined;
    },
    set: (base, name, value) => {
      if (name in base) {
        base[name] = value;
        return true;
      }
      if (STYLE_PROPERTY.test(String(name))) {
        properties.set(String(name), String(value));
        return true;
      }
      return Reflect.set(base, name, value);
    },
    deleteProperty: (base, name) => {
      properties.delete(String(name));
      return Reflect.deleteProperty(base, name);
    },
    has: (base, name) => name in base || properties.has(String(name)),
    ownKeys: () => [...new Set([...Reflect.ownKeys(target), ...properties.keys()])],
    getOwnPropertyDescriptor: () => ({ configurable: true, enumerable: true, value: "" })
  });
}

function createNode(tagName, registry) {
  const attributes = new Map();
  const listeners = new Map();
  const styles = new Map();
  let ownText = "";
  let markup = "";

  const node = {
    tagName: tagName.toUpperCase(),
    attributes,
    children: [],
    dataset: createDataset(),
    styles,
    value: "",
    blur() {
      if (registry.document.activeElement === node) registry.document.activeElement = null;
    },
    addEventListener(type, handler) {
      if (!listeners.has(type)) listeners.set(type, []);
      listeners.get(type).push(handler);
    },
    append(...children) {
      children.forEach((child) => {
        // A page may append a bare string, which a browser turns into a text run.
        const entry = typeof child === "string" || typeof child === "number" ? textRun(child) : child;
        node.children.push(entry);
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
    hasAttribute: (name) => attributes.has(name),
    querySelector(selector) {
      return node.querySelectorAll(selector)[0] || null;
    },
    querySelectorAll(selector) {
      rejectUnsupported(selector);
      return descendants(node).filter((entry) => selectorMatches(entry, selector));
    },
    replaceChildren(...children) {
      ownText = "";
      node.children = [];
      node.append(...children);
    },
    setAttribute(name, value) {
      attributes.set(name, String(value));
    }
  };

  node.id = "";
  node.click = (event = {}) => node.dispatch("click", event);

  // `hidden`, `disabled` and `checked` are attributes the IDL reflects, so the
  // page's own markup and the property a test reads cannot disagree.
  BOOL_PROPERTIES.forEach((property) => {
    Object.defineProperty(node, property, {
      configurable: true,
      get: () => attributes.has(property === "readOnly" ? "readonly" : dashCase(property)),
      set: (value) => {
        const name = property === "readOnly" ? "readonly" : dashCase(property);
        if (value) attributes.set(name, "");
        else attributes.delete(name);
      }
    });
  });

  const classes = () =>
    (attributes.get("class") || "").split(/\s+/).filter(Boolean);

  Object.defineProperty(node, "className", {
    configurable: true,
    get: () => attributes.get("class") || "",
    set: (value) => {
      const text = String(value ?? "").trim();
      if (text) attributes.set("class", text);
      else attributes.delete("class");
    }
  });

  node.classList = {
    add(...names) {
      node.className = [...new Set([...classes(), ...names.flatMap((n) => String(n).split(/\s+/))])].join(" ");
    },
    contains: (name) => classes().includes(name),
    remove(...names) {
      const drop = new Set(names.flatMap((n) => String(n).split(/\s+/)));
      node.className = classes().filter((name) => !drop.has(name)).join(" ");
    },
    toggle(name, force) {
      const on = force === undefined ? !classes().includes(name) : Boolean(force);
      if (on) node.classList.add(name);
      else node.classList.remove(name);
      return on;
    }
  };

  // Reading `textContent` is how a test checks what a person would read, so it
  // has to include the text of the children, not just the text set on this node.
  Object.defineProperty(node, "textContent", {
    configurable: true,
    get: () => ownText + node.children.map((child) => child.textContent || "").join(""),
    set: (value) => {
      node.children = [];
      ownText = String(value ?? "");
    }
  });

  Object.defineProperty(node, "innerHTML", {
    configurable: true,
    set(value) {
      markup = String(value ?? "");
      ownText = "";
      node.children = [];
      appendFragment(node, markup, registry);
    },
    get: () => markup + node.children.map((child) => child.markup || "").join("")
  });

  node.style = createStyle(styles);

  return node;
}

// Parsing the markup on assignment is what lets a page query into what it
// painted, instead of each test hand-parsing the selectors its own page uses.
function appendFragment(host, html, registry) {
  const stack = [host];
  let cursor = 0;

  for (const match of html.matchAll(TAG)) {
    const top = stack[stack.length - 1];
    const text = html.slice(cursor, match.index);
    if (text) top.append(textRun(text));

    cursor = match.index + match[0].length;
    if (match[1]) {
      if (stack.length > 1) stack.pop();
      continue;
    }

    const child = createNode(match[2], registry);
    for (const [name, value] of readAttributes(match[3])) {
      child.setAttribute(name, value);
      if (name === "id") {
        child.id = value;
        registry.byId.set(value, child);
      } else if (name.startsWith("data-")) {
        child.dataset[camelCase(name.slice(5))] = value;
      }
    }
    child.markup = match[0];
    top.append(child);
    if (!match[4] && !VOID.has(match[2].toLowerCase())) stack.push(child);
  }

  const tail = html.slice(cursor);
  if (tail) stack[stack.length - 1].append(textRun(tail));
}

function rejectUnsupported(selector) {
  if (!SUPPORTED_SELECTOR.test(selector)) {
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
  const match = selector.match(SUPPORTED_SELECTOR);
  if (!match) {
    throw new FakeDomError(`This fake DOM cannot answer the selector "${selector}".`);
  }
  const [, tag, rest = ""] = match;
  if (tag && tag !== "*" && node.tagName !== tag.toUpperCase()) return false;

  const parts = rest.match(/[#.][\w-]+|\[[^\]]+\]/g) || [];
  return parts.every((part) => {
    if (part.startsWith("#")) return node.id === part.slice(1);
    if (part.startsWith(".")) return node.classList.contains(part.slice(1));
    const [, name, , value] = part.match(/^\[([\w:-]+)(?:([~|^$*]?)=)?([^\]]*)\]$/);
    if (value === undefined) return node.hasAttribute(name);
    return node.getAttribute(name) === value.replace(/^"|"$/g, "");
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

// A `src` that the page means as a local asset: a relative path, optionally with a
// query string or fragment. Anything absolute, protocol-relative or remote is
// refused, because the harness has no way to fetch it and a test that silently ran
// without a module would pass against a page that cannot load one in the app.
const LOCAL_SCRIPT = /^(?!\/)(?![\w-]+:)([^"'\s?#]+)(?:[?#][^"'\s]*)?$/;

/**
 * Read one page and the scripts it runs, in document order.
 *
 * The shipped pages carry their JavaScript in external files, because the app's
 * script CSP allows no inline script (slugtale-9bx follow-up). So the harness
 * follows `src` the way a browser would for a local file — in the order the page
 * lists them, since that order decides who may call whom at load time.
 *
 * @param {string} file page name under `src/`, or an absolute path when `baseDir`
 *   says the page's references resolve somewhere else.
 * @param {string|undefined} markup page source to use instead of reading `src/`
 * @param {URL} baseDir directory the page's own relative `src` values resolve
 *   against; defaults to `src/`
 */
function readPage(file, markup, baseDir = SRC) {
  const html = markup ?? readFileSync(new URL(file, baseDir), "utf8");

  const scripts = [];
  for (const match of html.matchAll(SCRIPT)) {
    const src = /\bsrc\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/.exec(match.groups.attrs);
    if (src) {
      const value = src[1] ?? src[2] ?? src[3];
      if (!LOCAL_SCRIPT.test(value)) {
        throw new Error(
          `${file} loads "${value}" from outside src/. This harness runs local ` +
            `script files only, so a test here would pass against a page that ` +
            `cannot load that script in the app.`,
        );
      }
      scripts.push(readFileSync(new URL(value, baseDir), "utf8"));
      continue;
    }
    // An inline block is still run: a page may legitimately hold a few lines, and
    // removing the CSP's unsafe-inline is only worth doing once the shipped pages
    // actually stop relying on it.
    if (match.groups.body.trim()) scripts.push(match.groups.body);
  }
  if (scripts.length === 0) throw new Error(`${file} runs no script this harness can read`);

  return { script: scripts.join("\n;\n"), shell: html.replace(SCRIPT, "") };
}

function buildDocument(page) {
  const byId = new Map();
  const documentListeners = new Map();
  const registry = { byId, document: null };

  // Parse the page's own shell, so `document.body` and `document.head` are the
  // ones the page ships — `dictation-bar.html` puts its bar position on its body.
  const parsed = createNode("html", registry);
  appendFragment(parsed, page.shell, registry);

  const html = descendants(parsed).find((node) => node.tagName === "HTML") || parsed;
  const head = descendants(parsed).find((node) => node.tagName === "HEAD");
  const body = descendants(parsed).find((node) => node.tagName === "BODY") || createNode("body", registry);

  const document = {
    activeElement: null,
    body,
    createElement: (tagName) => createNode(tagName, registry),
    documentElement: html,
    head: head || null,
    blur() {
      if (registry.document.activeElement === node) registry.document.activeElement = null;
    },
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
      return descendants(html).filter((node) => selectorMatches(node, selector));
    }
  };
  registry.document = document;

  return { byId, document, rootStyle: html.styles };
}

/**
 * The CSS a browser would apply to a page: every local stylesheet it links, in
 * the order the page lists them.
 *
 * A test that asserts on styling has to read this rather than the page's markup.
 * The shipped pages keep their styles in files, so a `<style>`-block search finds
 * nothing and, worse, would keep finding nothing if the page ever stopped linking
 * the stylesheet at all.
 *
 * @param {string} file page name under `src/`
 * @returns {string} the concatenated stylesheets
 */
export function stylesheetOf(file) {
  const html = readFileSync(new URL(file, SRC), "utf8");
  const linked = [...html.matchAll(/<link[^>]*\brel="stylesheet"[^>]*>/g)].map(([tag]) => {
    const href = /\bhref\s*=\s*"([^"]*)"/.exec(tag);
    return href && LOCAL_SCRIPT.test(href[1]) ? href[1] : null;
  });
  if (linked.length === 0) {
    throw new Error(`${file} links no local stylesheet, so it ships unstyled`);
  }
  return linked.map((href) => readFileSync(new URL(href, SRC), "utf8")).join("\n");
}

/**
 * Run a page against a fake DOM built from the page's own markup.
 *
 * @param {string} file            page name under `src/`, e.g. `index.html`
 * @param {object} [options]
 * @param {string} [options.markup]  page source to run instead of reading `src/`
 * @param {URL} [options.baseDir]  directory a page's own relative `src` values
 *   resolve against, for a fixture page that lives outside `src/`
 * @param {Function} [options.invoke]  stands in for the Tauri bridge
 * @param {string[]} [options.exports]  handles the test will read, and that the page must expose
 * @param {string} [options.exportSource]  raw export block, for a handle that closes over private state
 * @param {string[]} [options.bootstrap]  top-level calls to strip, default `["init"]`
 * @param {boolean} [options.runBootstrap]  keep the bootstrap call instead of stripping it
 * @param {string} [options.userAgent]
 * @param {number|Function} [options.now]  what the page's `Date.now()` returns
 * @param {boolean} [options.reduceMotion]  what `matchMedia` reports
 * @param {string} [options.exportName]  where the handles are published
 * @returns {{api: object, document: object, elements: Map, intervals: Array,
 *   timeouts: Array, rootStyle: Map, flushNextTimeout: Function, tickIntervals: Function,
 *   keydown: Function, windowEvent: Function}}
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

  const page = readPage(file, options.markup, options.baseDir);
  const { byId, document, rootStyle } = buildDocument(page);

  // Timers are always queued and never fire on their own. A test that needs one
  // to happen asks for it, so a page cannot pass a test by waiting and fail for
  // a real user, or the other way round.
  const timeouts = [];
  const intervals = [];
  const cleared = new Set();
  const timers = {
    clearInterval(handle) {
      cleared.add(handle);
      const index = intervals.findIndex((entry) => entry.handle === handle);
      if (index >= 0) intervals.splice(index, 1);
    },
    setInterval(callback, delay) {
      const handle = intervals.length + 1 + cleared.size;
      intervals.push({ callback, delay, handle });
      return handle;
    },
    setTimeout(callback, delay) {
      timeouts.push({ callback, delay });
      return timeouts.length;
    }
  };

  // Strip the bootstrap calls wherever they sit on a line of their own, so a page
  // can start itself from a second script block.
  let source = page.script;
  bootstrap.forEach((name) => {
    source = source.replace(new RegExp(`^\\s*${name}\\(\\);?\\s*$`, "gm"), "");
  });
  if (exportNames.length > 0) {
    source += `\nwindow.${exportName} = { ${options.exportSource || exportNames.join(", ")} };`;
  }

  const windowListeners = new Map();
  const window = {
    __TAURI__: { core: { invoke }, event: { listen() {} } },
    blur() {
      if (registry.document.activeElement === node) registry.document.activeElement = null;
    },
    addEventListener(type, handler) {
      if (!windowListeners.has(type)) windowListeners.set(type, []);
      windowListeners.get(type).push(handler);
    },
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
  if (now !== undefined) {
    // Subclass the realm's own Date rather than handing the page a host object, so
    // `new Date(x) instanceof Date` and the prototype chain still hold. The reader
    // is looked up on each call, so a test can move the clock between reads.
    context.readPinnedNow = typeof now === "function" ? now : () => now;
    vm.runInNewContext(
      `const read = globalThis.readPinnedNow;
       const RealDate = Date;
       globalThis.Date = class extends RealDate {
         constructor(...args) { super(...(args.length ? args : [read()])); }
         static now() { return read(); }
       };
       delete globalThis.readPinnedNow;`,
      context,
      { filename: file },
    );
  }
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
    rootStyle,
    timeouts,
    // A page queues its next timer from inside a promise callback, so waiting for
    // one to appear is part of flushing one.
    async flushNextTimeout() {
      for (let spin = 0; timeouts.length === 0; spin += 1) {
        if (spin >= 20) throw new Error(`No pending timer after ${spin} turns of the event loop`);
        await Promise.resolve();
      }
      timeouts.shift().callback();
    },
    // Fire every interval the page still has running, the way a real clock would.
    tickIntervals(...args) {
      intervals.forEach(({ callback }) => callback(...args));
    },
    keydown(key, event = {}) {
      document.dispatch("keydown", { key, ...event });
    },
    windowEvent(type, event = {}) {
      (windowListeners.get(type) || []).forEach((handler) => handler(event));
    }
  };
}
