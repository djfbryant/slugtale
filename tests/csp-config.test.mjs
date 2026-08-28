import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const config = require("../src-tauri/tauri.conf.json");

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
    scriptSrc.includes("'self'") && scriptSrc.includes("'unsafe-inline'"),
    "the runtime frontend is inline scripts, so script-src keeps 'unsafe-inline' next to 'self'",
  );

  const connectSrc = directives.get("connect-src");
  assert.ok(connectSrc.includes("ipc:"), "Tauri IPC needs the ipc: origin");
});
