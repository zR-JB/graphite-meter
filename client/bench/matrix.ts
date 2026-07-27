// The cells, the order they run in, and the statistics used to read them.
import type { CellSpec, CellResult } from "./harness";

export const REPS = Number(process.env.GM_BENCH_REPS ?? 3);
export const WARMUP_MS = Number(process.env.GM_BENCH_WARMUP_MS ?? 3000);
export const MEASURE_MS = Number(process.env.GM_BENCH_MEASURE_MS ?? 8000);
export const SEED = Number(process.env.GM_BENCH_SEED ?? 1);

export interface Cell {
  id: string;
  /** Axis being swept, so the report groups a sweep together. */
  group: string;
  spec: Omit<CellSpec, "warmupMs" | "measureMs">;
}

type Origins = Record<string, string>;

/** Lane counts, the sweep that bounds how many are useful. Upload stops at 8:
 *  beyond that the per-lane pool falls below a useful POST size. */
const DOWN_LANES = [1, 2, 4, 6, 8, 12, 16];
const UP_LANES = [1, 2, 3, 4, 6, 8];

export function buildCells(origins: Origins): Cell[] {
  const cells: Cell[] = [];
  const at = (group: string, id: string, spec: Cell["spec"]): void =>
    void cells.push({ group, id, spec });

  const laneSweep = (
    origin: string,
    name: string,
    transport: CellSpec["transport"],
    down: number[],
    up: number[],
    bootstrapH3 = false,
  ): void => {
    for (const lanes of down)
      at(`${name}/down/lanes`, `${name}/down/lanes=${lanes}`, {
        origin,
        dir: "down",
        transport,
        lanes,
        bootstrapH3,
      });
    for (const lanes of up)
      at(`${name}/up/lanes`, `${name}/up/lanes=${lanes}`, {
        origin,
        dir: "up",
        transport,
        lanes,
        bootstrapH3,
      });
  };

  for (const [name, origin] of Object.entries(origins)) {
    // h1 clear carries the full sweep; the others only need enough points to
    // place their curve against it.
    const full = name === "h1-clear";
    laneSweep(
      origin,
      name,
      "fetch-stream",
      full ? DOWN_LANES : [1, 2, 4, 8],
      full ? UP_LANES : [1, 2, 4],
      name === "h3",
    );
    // WebTransport rides h3 only, and a datagram flood opens no lanes at all.
    if (name !== "h3") continue;
    // These ride the h3 origin, and an upload cell mints its id over it before
    // the session is dialed, so they need the same upgrade the fetch cells do.
    laneSweep(origin, "wt", "webtransport", [1, 2, 4, 8, 16], [1, 2, 4], true);
    laneSweep(origin, "wtdg", "webtransport-datagram", [1], [1], true);
  }

  return cells;
}

/** Deterministic shuffle, so a session's order is reproducible from its seed. */
export function shuffled<T>(items: T[], seed: number): T[] {
  const out = items.slice();
  let state = seed >>> 0 || 1;
  const next = (): number => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 4294967296;
  };
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(next() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

export function gbps(r: CellResult): number {
  return r.elapsedMs > 0 ? (r.bytes * 8) / (r.elapsedMs / 1000) / 1e9 : 0;
}

/** Nearest-rank percentile: no interpolation between samples that were never taken. */
export function pct(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const i = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length));
  return sorted[i];
}

export interface Summary {
  n: number;
  median: number;
  p25: number;
  p75: number;
  min: number;
  max: number;
  /** Spread within one run, over its rate buckets. High means it oscillated. */
  cv: number;
  maxTickMs: number;
  /** Runs whose page stalled. Those measure the engine giving up, not the link,
   *  so they are counted rather than averaged in silently. */
  stalled: number;
}

/** A tick this late means the page could not keep up, so the run is not a
 *  measurement of the path. */
export const STALL_TICK_MS = 600;

export function summarise(runs: CellResult[]): Summary {
  const rates = runs.map(gbps).sort((a, b) => a - b);
  const cvs = runs.map((r) => {
    const bucketRates = r.buckets
      .filter((b) => b.ms > 0)
      .map((b) => (b.bytes * 8) / (b.ms / 1000) / 1e9);
    if (bucketRates.length < 2) return 0;
    const mean = bucketRates.reduce((a, b) => a + b, 0) / bucketRates.length;
    if (mean <= 0) return 0;
    const varr =
      bucketRates.reduce((a, b) => a + (b - mean) ** 2, 0) / bucketRates.length;
    return Math.sqrt(varr) / mean;
  });
  return {
    n: runs.length,
    median: pct(rates, 50),
    p25: pct(rates, 25),
    p75: pct(rates, 75),
    min: rates[0] ?? 0,
    max: rates[rates.length - 1] ?? 0,
    cv: cvs.sort((a, b) => a - b)[Math.floor(cvs.length / 2)] ?? 0,
    maxTickMs: Math.max(...runs.map((r) => r.maxTickMs), 0),
    stalled: runs.filter((r) => r.maxTickMs > STALL_TICK_MS).length,
  };
}
