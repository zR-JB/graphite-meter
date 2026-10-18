import { afterAll, expect, test } from "bun:test";

const workers = {
  clock: "/assets/clock-1.js",
  lane: "/assets/lane-2.js",
  session: "/assets/session-3.js",
  probe: "/assets/probe-4.js",
};
const created: string[] = [];
Object.assign(globalThis, {
  WORKERS: workers,
  trustedTypes: { createPolicy: (name: string, rules: unknown) => (created.push(name), rules) },
});
afterAll(() => {
  Reflect.deleteProperty(globalThis, "WORKERS");
  Reflect.deleteProperty(globalThis, "trustedTypes");
});
// A module instance of its own, which creates its policy under the fake factory above.
const fresh = "./trusted?policy";
const { TRUSTED_TYPES_POLICY, workerScript }: typeof import("./trusted") = await import(fresh);

test("the page's one policy admits the build's workers and nothing else", () => {
  expect(created).toEqual([]);
  for (const url of Object.values(workers)) expect(workerScript(url)).toBe(url);
  expect(created).toEqual([TRUSTED_TYPES_POLICY]);
  for (const url of [
    "/assets/other.js",
    "https://elsewhere.example/assets/lane-2.js",
    "data:text/javascript,",
  ])
    expect(() => workerScript(url)).toThrow(TypeError);
});
