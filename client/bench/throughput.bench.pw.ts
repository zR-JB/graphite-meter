// Browser throughput benchmark. Run with `just bench-throughput`; never in ci.
//
// Findings and their interpretation belong in docs/BENCHMARKS.md; this writes
// the raw rows they are computed from. Cells run in a fresh permutation each
// repeat round, so drift across a session inflates spread rather than biasing
// one cell.
import { test, expect } from "@playwright/test";
import { appendFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { origins } from "../playwright.bench.config";
import type { CellSpec, CellResult } from "./harness";
import {
  buildCells,
  shuffled,
  summarise,
  gbps,
  REPS,
  SEED,
  WARMUP_MS,
  MEASURE_MS,
} from "./matrix";

/** Only origins the harness can actually reach are measured; the rest are
 *  recorded as invalid rather than left as a blank the reader must interpret. */
const enabled = (process.env.GM_BENCH_ORIGINS ?? "h1-clear").split(",");
const active = Object.fromEntries(
  Object.entries(origins).filter(([name]) => enabled.includes(name)),
);

const cells = buildCells(active);
const DIR = "bench/results";

/** Appended as each run completes. A failing test restarts the worker and
 *  resets module state, so anything held only in memory is lost with it. */
function record(
  project: string,
  cell: string,
  group: string,
  r: CellResult,
): void {
  mkdirSync(DIR, { recursive: true });
  appendFileSync(
    `${DIR}/${project}.ndjson`,
    JSON.stringify({
      schemaVersion: 1,
      project,
      cell,
      group,
      gbps: gbps(r),
      bytes: r.bytes,
      elapsedMs: r.elapsedMs,
      laneBytes: r.laneBytes,
      maxTickMs: r.maxTickMs,
      tuning: r.tuning,
      errors: r.errors,
    }) + "\n",
  );
}

async function runCell(
  page: import("@playwright/test").Page,
  spec: CellSpec,
): Promise<CellResult> {
  await page.goto("/bench/harness.html");
  // playwright.bench.config.ts sets GM_CLIENT_BENCH=1, but only on a dev server
  // it starts itself: reuseExistingServer hands back whatever is already on the
  // port. Without the surface the workers discard every `tune` and each cell
  // measures DEFAULT_TUNING while the row records the tuning that was asked for,
  // so the run fails here rather than producing wrong numbers.
  expect(
    await page.evaluate(() => window.__gmBench.tuningSurface),
    "the dev server on :5173 was built without GM_CLIENT_BENCH=1; restart it or let Playwright start it",
  ).toBe(true);
  return page.evaluate((s) => window.__gmBench.run(s), spec);
}

test.describe("matrix", () => {
  for (let rep = 1; rep <= REPS; rep++) {
    for (const cell of shuffled(cells, SEED + rep)) {
      test(`${cell.id} r${rep}`, async ({ page }) => {
        const result = await runCell(page, {
          ...cell.spec,
          warmupMs: WARMUP_MS,
          measureMs: MEASURE_MS,
        });
        record(test.info().project.name, cell.id, cell.group, result);
        // A run that carried nothing is the engine stalling, which is a fact
        // about the engine. Only a lane reporting an error is a broken cell.
        expect(result.errors).toEqual([]);
      });
    }
  }
});

test.afterAll(() => {
  const project = test.info().project.name;
  const path = `${DIR}/${project}.ndjson`;
  if (!existsSync(path)) return;
  const rows = readFileSync(path, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line) as Record<string, unknown>);

  const byCell = new Map<string, CellResult[]>();
  const groups = new Map<string, string>();
  for (const row of rows) {
    const cell = row.cell as string;
    groups.set(cell, row.group as string);
    byCell.set(cell, [
      ...(byCell.get(cell) ?? []),
      {
        bytes: row.bytes as number,
        elapsedMs: row.elapsedMs as number,
        laneBytes: row.laneBytes as number[],
        buckets: [],
        maxTickMs: row.maxTickMs as number,
        tuning: row.tuning as never,
        errors: row.errors as string[],
      },
    ]);
  }

  const byGroup = new Map<string, string[]>();
  for (const [id, list] of byCell) {
    const s = summarise(list);
    const line = `  ${id.padEnd(32)} n=${s.n} median=${s.median.toFixed(2)} p25=${s.p25.toFixed(2)} p75=${s.p75.toFixed(2)} min=${s.min.toFixed(2)} max=${s.max.toFixed(2)} stalled=${s.stalled}/${s.n} maxTick=${s.maxTickMs.toFixed(0)}ms`;
    const group = groups.get(id) ?? "other";
    byGroup.set(group, [...(byGroup.get(group) ?? []), line]);
  }
  const out = [`\n=== ${project} (Gbit/s, seed ${SEED}, ${REPS} reps) ===`];
  for (const [group, lines] of [...byGroup].sort())
    out.push(`${group}:`, ...lines.sort());
  console.log(out.join("\n"));
});
