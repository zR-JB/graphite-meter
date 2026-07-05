import { test, expect } from "bun:test";

// RealRunner.ts imports buildenv.ts, whose `BUILD` object reads `__GM_*__`
// globals that Vite's `define` normally inlines at bundle time (see
// vite.config.ts). Plain `bun test` never runs that step, so stub the
// globals before a dynamic import evaluates the module.
Object.assign(globalThis, {
  __GM_DEFAULT_ENGINE__: "real",
  __GM_ALLOW_DUMMY__: true,
  __GM_DEV_TOOLS__: true,
  __GM_BUILD_LABEL__: "test",
  __GM_CLIENT_VERSION__: "0.0.0+test",
});

const { resolveBase, httpToWs, wsToWss, median } = await import("./RealRunner");

test("resolveBase: explicit endpoint override builds an absolute origin", () => {
  expect(resolveBase({ host: "example.com", port: 8080 })).toBe(
    "http://example.com:8080",
  );
  expect(resolveBase({ host: "example.com", port: 443 })).toBe(
    "https://example.com:443",
  );
});

test("resolveBase: no endpoint (or auto/empty host) derives same-origin", () => {
  expect(resolveBase(undefined)).toBe("");
  expect(resolveBase({ host: "auto", port: 0 })).toBe("");
  expect(resolveBase({ host: "", port: 0 })).toBe("");
});

test("httpToWs: maps an http(s) origin to its ws(s) equivalent", () => {
  expect(httpToWs("http://example.com:8080")).toBe("ws://example.com:8080");
  expect(httpToWs("https://example.com")).toBe("wss://example.com");
});

test("httpToWs: an already-ws(s) or relative input passes through unchanged", () => {
  expect(httpToWs("ws://example.com:8080")).toBe("ws://example.com:8080");
  expect(httpToWs("")).toBe("");
});

test("wsToWss: upgrades a ws:// base to wss://", () => {
  expect(wsToWss("ws://example.com:8080")).toBe("wss://example.com:8080");
});

test("wsToWss: an already-wss:// base passes through unchanged", () => {
  expect(wsToWss("wss://example.com:8080")).toBe("wss://example.com:8080");
});

test("median: empty input is NaN (callers must guard on length before calling)", () => {
  expect(median([])).toBeNaN();
});

test("median: a single-value array returns that value", () => {
  expect(median([7])).toBe(7);
});

test("median: an even-length array averages the two middle values", () => {
  expect(median([1, 3, 5, 7])).toBe(4);
});

test("median: an odd-length array returns the middle value, unordered input", () => {
  expect(median([5, 1, 3])).toBe(3);
});
