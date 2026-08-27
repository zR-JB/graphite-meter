import { test, expect, origins, harnessOrigin } from "./fixtures";
import type { CellSpec, CellResult } from "../bench/harness";
import type { Page } from "../browser/webview";
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
    name: "fetch streams upload",
    spec: {
      origin: origins["h1-clear"],
      dir: "up",
      transport: "fetch-stream",
      lanes: 1,
    },
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
  },
  {
    name: "WebTransport streams upload",
    spec: {
      origin: origins.h3,
      dir: "up",
      transport: "webtransport",
      lanes: 1,
    },
  },
  {
    name: "WebTransport datagram download",
    spec: {
      origin: origins.h3,
      dir: "down",
      transport: "webtransport-datagram",
      lanes: 1,
    },
  },
  {
    name: "WebTransport datagram upload",
    spec: {
      origin: origins.h3,
      dir: "up",
      transport: "webtransport-datagram",
      lanes: 1,
    },
  },
];
async function runCell(
  page: Page,
  spec: Omit<CellSpec, "warmupMs" | "measureMs">,
): Promise<CellResult> {
  await page.goto(`${harnessOrigin}/bench/harness.html`);
  return page.evaluate((cell) => window.__gmBench.run(cell), {
    ...spec,
    warmupMs: WARMUP_MS,
    measureMs: MEASURE_MS,
  } as CellSpec);
}
for (const { name, spec } of cells) {
  test(`${name} carries bytes end to end`, async ({ page }) => {
    const result = await runCell(page, spec);
    expect(result.errors).toEqual([]);
    expect(result.bytes).toBeGreaterThan(0);
  });
}
