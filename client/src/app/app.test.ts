import { afterEach, expect, jest, test } from "bun:test";
import type { HistoryRecord } from "./history/records";

const GLOBALS = ["window", "document", "location", "navigator", "indexedDB", "isSecureContext", "BUILD"];
const saved = new Map(GLOBALS.flatMap((name) => (name in globalThis ? [[name, Reflect.get(globalThis, name)]] : [])));
afterEach(() => {
  jest.useRealTimers();
  for (const name of GLOBALS)
    if (saved.has(name)) Reflect.set(globalThis, name, saved.get(name));
    else Reflect.deleteProperty(globalThis, name);
});

/** A signed-in page whose History saves results, on storage that never answers; where it navigated to. */
function signedInPage(): string[] {
  jest.useFakeTimers();
  const navigated: string[] = [];
  const meta: Record<string, string> = {
    "graphite-meter-auth": "enabled",
    "graphite-meter-result-history-default": "true",
  };
  Object.assign(globalThis, {
    BUILD: { clientVersion: "v9.9.9" },
    window: new EventTarget(),
    document: {
      querySelector: (selector: string) => {
        const content = meta[/name="([^"]+)"/.exec(selector)?.[1] ?? ""];
        return content ? { getAttribute: () => content } : null;
      },
      visibilityState: "visible",
      cookie: "",
      addEventListener() {},
      removeEventListener() {},
    },
    location: { origin: "https://meter.example", protocol: "https:", replace: (url: string) => navigated.push(url) },
    navigator: { onLine: true },
    indexedDB: { open: () => ({}) },
    isSecureContext: true,
  });
  return navigated;
}

const { createApp } = await import("./app");
const settle = async () => {
  for (let turn = 0; turn < 20; turn++) await null;
};

test("an ended sign-in leaves for sign-in once the result is saved, waiting at most 3 s", async () => {
  const navigated = signedInPage();
  const app = createApp();
  app.saver.offer({ id: "r" } as HistoryRecord);
  expect(app.saver.pending()).toBe(true);
  app.session.end("expired");
  expect(app.signingOut()).toBe(true);
  jest.advanceTimersByTime(2_999);
  await settle();
  expect(navigated).toEqual([]);
  jest.advanceTimersByTime(1);
  await settle();
  expect(navigated).toEqual(["/login?reason=expired"]);
});

test("signing out waits for nothing pending, and the sign-in it ends is no reason to leave", async () => {
  const navigated = signedInPage();
  const app = createApp();
  await app.signOut();
  app.session.end("expired");
  await settle();
  expect([navigated, app.signingOut()]).toEqual([[], true]);
  app.stay();
  expect(app.signingOut()).toBe(false);
  app.dispose();
});
