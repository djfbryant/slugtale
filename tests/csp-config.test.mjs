import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

import { shippedSource } from "./harness.mjs";

const require = createRequire(import.meta.url);
const config = require("../src-tauri/tauri.conf.json");

const RUNTIME_PAGES = ["dictation-bar.html", "index.html", "typing-challenge.html"];

test("the app windows ship a CSP that blocks remote loads", () => {
  const csp = config.app?.security?.csp;
  assert.ok(typeof csp === "string" && csp.length > 0, "app.security.csp must be set");

  const directives = new Map(
    csp.split(";").map((directive) => {
      const [name, ...values] = directive.trim().split(/\s+/);
      return [name, values];
    }),
  );

  assert.equal(directives.get("default-src")?.[0], "'self'");
  for (const name of ["script-src", "style-src", "img-src", "connect-src"]) {
    const values = directives.get(name);
    assert.ok(values, `${name} must be pinned explicitly`);
    assert.ok(
      !values.some(
        (value) => ( /^https?:/.test(value) && value !== "http://ipc.localhost" ) || value === "*",
      ),
      `${name} must not allow remote origins (the local ipc loopback is required)`,
    );
  }

  const scriptSrc = directives.get("script-src");
  assert.ok(
    scriptSrc.includes("'self'") && !scriptSrc.includes("'unsafe-inline'"),
    "the shipped pages load no inline script, so script-src does not need 'unsafe-inline'",
  );

  const connectSrc = directives.get("connect-src");
  assert.ok(connectSrc.includes("ipc:"), "Tauri IPC needs the ipc: origin");
});

// `'unsafe-inline'` in a script policy is not a limit, it is the absence of one: a
// bug in any page becomes script this window will run. The pages carry their code
// in files, so the policy can be exact, and these are the tests that make dropping
// the escape hatch safe rather than merely desirable.
test("no shipped page carries an inline script", () => {
  for (const page of RUNTIME_PAGES) {
    const { markup } = shippedSource(page);
    const inline = [...markup.matchAll(/<script(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/g)]
      .map(([, body]) => body)
      .filter((body) => body.trim().length > 0);

    assert.deepEqual(
      inline.map((body) => body.slice(0, 60)),
      [],
      `${page} runs inline script, which script-src 'self' without 'unsafe-inline' blocks`,
    );
  }
});

test("every script a page runs is a file it ships", () => {
  for (const page of RUNTIME_PAGES) {
    const { markup } = shippedSource(page);
    const tags = [...markup.matchAll(/<script[^>]*>/g)].map(([tag]) => tag);

    for (const tag of tags) {
      assert.match(
        tag,
        /\bsrc\s*=\s*"[^"]+"/,
        `${page} has a script tag with no src, so it runs inline`,
      );
      assert.doesNotMatch(
        tag,
        /\b(?:https?:)?\/\//,
        `${page} loads a script from a remote origin, which the CSP blocks and the app must not need`,
      );
    }
    assert.ok(tags.length > 0, `${page} must still run its script`);
  }
});
