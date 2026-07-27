// Does a real browser move bytes over each transport, against a real server?
// Nothing else answers that: the rest of e2e never reaches a backend, and the
// worker suites mock the session.
//
// A contract check, not a measurement. It asserts that bytes moved and that no
// lane reported an error — never a rate, which on a shared runner is noise.
import { test, expect } from "@playwright/test";
import { origins } from "../playwright.live.config";
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
}[] = [
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
  },
];

for (const { name, spec } of cells) {
  test(`${name} carries bytes end to end`, async ({ page }) => {
    await page.goto("/bench/harness.html");
    const result = await page.evaluate((cell) => window.__gmBench.run(cell), {
      ...spec,
      warmupMs: WARMUP_MS,
      measureMs: MEASURE_MS,
    } as CellSpec);
    expect(result.errors).toEqual([]);
    expect(result.bytes).toBeGreaterThan(0);
  });
}
