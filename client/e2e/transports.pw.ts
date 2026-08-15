// Does a real browser move bytes over each transport, against a real server?
// Nothing else answers that: the browser suite never reaches a backend, and the
// worker suites mock the session.
//
// A contract check, not a measurement. It asserts that bytes moved and that no
// lane reported an error — never a rate, which on a shared runner is noise.
import { test, expect } from "./fixtures";
import { origins } from "../playwright.e2e.config";
import type { CellSpec, CellResult } from "../bench/harness";

declare global {
  interface Window {
    __gmBench: { run: (spec: CellSpec) => Promise<CellResult> };
  }
}

/** Enough to prove lanes establish and carry, short enough to stay cheap. */
const WARMUP_MS = 250;
const MEASURE_MS = 750;

const cells: {
  name: string;
  spec: Omit<CellSpec, "warmupMs" | "measureMs">;
  projects?: string[];
}[] = [
  {
    // Firefox can exercise the complete HTTP/1 upload + authoritative progress
    // path without needing the throwaway HTTP/3 certificate trust setup.
    name: "fetch streams upload",
    spec: {
      origin: origins["h1-clear"],
      dir: "up",
      transport: "fetch-stream",
      lanes: 1,
    },
    projects: ["chromium", "firefox"],
  },
  {
    name: "fetch streams download",
    spec: {
      origin: origins["h1-clear"],
      dir: "down",
      transport: "fetch-stream",
      lanes: 2,
    },
  },
  {
    name: "WebTransport streams download",
    spec: {
      origin: origins.h3,
      dir: "down",
      transport: "webtransport",
      lanes: 1,
    },
    projects: ["chromium"],
  },
  {
    // The upload half is the one with moving parts: client-opened lanes, the
    // server-opened progress feed, and the finalizing DELETE. Its bytes are
    // the server's own count, so this also proves the feed arrived.
    name: "WebTransport streams upload",
    spec: {
      origin: origins.h3,
      dir: "up",
      transport: "webtransport",
      lanes: 1,
    },
    projects: ["chromium"],
  },
  // The datagram loops are the one path whose rate depends on how the worker
  // yields, so they are the reason workers/taskTurn.ts exists. Nothing else
  // drives them against a real session.
  {
    name: "WebTransport datagram download",
    spec: {
      origin: origins.h3,
      dir: "down",
      transport: "webtransport-datagram",
      lanes: 1,
    },
    projects: ["chromium"],
  },
  {
    name: "WebTransport datagram upload",
    spec: {
      origin: origins.h3,
      dir: "up",
      transport: "webtransport-datagram",
      lanes: 1,
    },
    projects: ["chromium"],
  },
];

for (const { name, spec, projects } of cells) {
  test(`${name} carries bytes end to end`, async ({ page, harnessOrigin }, testInfo) => {
    test.skip(
      projects !== undefined && !projects.includes(testInfo.project.name),
      "this transport is not available in this real-browser project",
    );
    await page.goto(`${harnessOrigin}/bench/harness.html`);
    const result = await page.evaluate((cell) => window.__gmBench.run(cell), {
      ...spec,
      warmupMs: WARMUP_MS,
      measureMs: MEASURE_MS,
    } as CellSpec);
    expect(result.errors).toEqual([]);
    expect(result.bytes).toBeGreaterThan(0);
  });
}
