// Coordinated throughput (docs/MEASUREMENTS.md#coordinated-servers, api/aggregation.testvectors.json).
import type { TransferStage } from "../run/schedule";
import type { Checkpoint, Direction } from "../types";
import {
  type Mark as SeriesMark,
  RateSeries,
  SCORE_BUCKETS,
  stillStable,
  transferConfidence,
} from "./stability";

/** Evidence must span this long on the client clock and on every receiver clock. */
export const MIN_EVIDENCE_MS = 800;
export const PEAK_WINDOW_MS = 500;
/** Recent intervals a result keeps; older ones only count. */
const INTERVAL_LIMIT = 128;
const COMBINED = "";

/** One instant across the participants: download counters and the latest receiver checkpoints. */
export interface Boundary {
  atMs: number;
  down: Record<string, number>;
  up: Record<string, Checkpoint | null>;
}

/** One server's share of a window, on the clock its evidence comes from. */
export interface ComponentWindow {
  serverId: string;
  bytes: number;
  durationMs: number;
  bytesPerSec: number;
  clock: "client-monotonic" | "receiver";
  startBytes: number;
  endBytes: number;
  startNanos?: number;
  endNanos?: number;
  startRequestMs?: number;
  startResponseMs?: number;
  endRequestMs?: number;
  endResponseMs?: number;
}

interface AggregateWindow {
  startMs: number;
  endMs: number;
  down: ComponentWindow[] | null;
  up: ComponentWindow[] | null;
  downBytesPerSec: number | null;
  upBytesPerSec: number | null;
  /** Time in the window when no server moved each direction. */
  quietMs?: Record<Direction, number>;
}

export interface Interval {
  id: number;
  stage: TransferStage;
  participants: string[];
  startMs: number;
  endMs: number;
  /** False once a replaced receiver or a regressed counter cut it short. */
  complete: boolean;
  reason: "stage-start" | "dropout" | "evidence-resumed";
  full: AggregateWindow | null;
  headline: AggregateWindow | null;
}

export interface ThroughputResult {
  /** The fastest disjoint window of at least 500 ms on every clock, never below the headline. */
  peakBytesPerSec: number;
  stabilityPct: number;
  /** Time in the headline window when no server moved this direction. */
  quietMs?: number;
  totalBytes: number;
  reportedBytesPerSec: number;
}

/** A stage direction's settled headline and the window it describes. */
interface Headline {
  result: ThroughputResult;
  window: AggregateWindow;
}

type Totals = Record<Direction, number>;
type Series = Record<Direction, RateSeries>;
type Peaks = Map<string, Partial<Record<Direction, number>>>;

/** Where a server last moved a direction: enough to end the interval there if it departs. */
interface Mark {
  boundary: Boundary;
  full: AggregateWindow | null;
  headline: AggregateWindow | null;
  peaks: Peaks;
  series: [RateSeries, SeriesMark][];
}

/** A result kept from an interval the log let go: what a stage or a server measured stays measured. */
interface Kept {
  window: AggregateWindow | null;
  bytesPerSec: number;
  peakBytesPerSec: number;
  stabilityPct: number;
}

interface Open {
  record: Interval;
  first: Boundary | null;
  last: Boundary | null;
  /** Where the current steady stretch began, while early finish tracks one. */
  stable: Boundary | null;
  wasStable: boolean;
  quiet: Totals;
  /** Quiet time up to `stable`. */
  stableQuiet: Totals;
  /** All directions together, for the early-finish score. */
  combined: RateSeries;
  total: Series;
  servers: Map<string, Series>;
  peakFrom: Boundary | null;
  peaks: Peaks;
  moved: Map<string, Partial<Record<Direction, Mark>>>;
}

export const directions = (stage: TransferStage): Direction[] =>
  stage === "bidirectional" ? ["down", "up"] : [stage === "download" ? "down" : "up"];

const rateOf = (window: AggregateWindow, dir: Direction) =>
  dir === "down" ? window.downBytesPerSec : window.upBytesPerSec;

const series = (): Series => ({ down: new RateSeries(), up: new RateSeries() });

export class Aggregate {
  intervals: Interval[] = [];
  omittedIntervals = 0;
  #open: Open | null = null;
  #closed = new Map<number, Open>();
  #stageTotals = new Map<TransferStage, Map<string, Totals>>();
  /** The highest byte count credited per server and upload id. */
  #ledgers = new Map<string, Map<string, number>>();
  /** Where each stage's first interval began, once the interval limit let it go. */
  #evictedStart = new Map<TransferStage, number>();
  /** The latest results of intervals the limit let go, by stage and direction, and by server. */
  #kept = new Map<string, Kept>();
  #latest: { boundary: Boundary; final: boolean } | null = null;

  get current(): Interval | null {
    return this.#open?.record ?? null;
  }

  /** Whether the open interval already holds a reportable window. */
  get sufficient(): boolean {
    return !!this.#open?.record.complete && sufficient(this.#open.record.full);
  }

  begin(
    stage: TransferStage,
    participants: string[],
    atMs: number,
    reason: Interval["reason"] = "stage-start",
  ): void {
    this.close();
    if (reason === "stage-start") {
      this.#ledgers.clear();
      this.#latest = null;
    }
    if (this.intervals.length >= INTERVAL_LIMIT) {
      const evicted = this.intervals.shift()!;
      this.#keep(evicted);
      this.#closed.delete(evicted.id);
      if (!this.#evictedStart.has(evicted.stage))
        this.#evictedStart.set(evicted.stage, evicted.startMs);
      this.omittedIntervals++;
    }
    const record: Interval = {
      id: this.omittedIntervals + this.intervals.length,
      stage,
      participants: [...participants],
      startMs: atMs,
      endMs: atMs,
      complete: true,
      reason,
      full: null,
      headline: null,
    };
    this.intervals.push(record);
    this.#open = {
      record,
      first: null,
      last: null,
      stable: null,
      wasStable: false,
      quiet: { down: 0, up: 0 },
      stableQuiet: { down: 0, up: 0 },
      combined: new RateSeries(SCORE_BUCKETS),
      total: series(),
      servers: new Map(participants.map((id) => [id, series()])),
      peakFrom: null,
      peaks: new Map(),
      moved: new Map(participants.map((id) => [id, {}])),
    };
  }

  /** Ends the interval where a departed server last moved, so its silence never counts; survivors go on from
      there in a `dropout` interval. */
  dropout(survivors: string[], atMs: number): void {
    const open = this.#open;
    if (!open || survivors.length === open.record.participants.length) return;
    const { stage } = open.record;
    let end: Mark | undefined;
    for (const [id, moved] of open.moved)
      if (!survivors.includes(id))
        for (const mark of Object.values(moved))
          if (!end || mark.boundary.atMs < end.boundary.atMs) end = mark;
    if (!end) {
      if (survivors.length) this.begin(stage, survivors, atMs, "dropout");
      return;
    }
    Object.assign(open.record, {
      endMs: end.boundary.atMs,
      full: end.full,
      headline: end.headline,
    });
    open.peaks = end.peaks;
    for (const [rates, mark] of end.series) rates.restore(mark);
    if (!survivors.length) return;
    const latest = this.#latest;
    this.begin(stage, survivors, end.boundary.atMs, "dropout");
    this.#start(this.#open!, end.boundary);
    if (latest && latest.boundary.atMs > end.boundary.atMs)
      this.observe(latest.boundary, latest.final);
  }

  close(): void {
    if (this.#open) this.#closed.set(this.#open.record.id, this.#open);
    this.#open = null;
  }

  /** Unique measured bytes per server, never counting overlapping evidence twice. */
  totals(id: string): Totals {
    const sum = { down: 0, up: 0 };
    for (const stage of this.#stageTotals.values())
      for (const dir of ["down", "up"] as const) sum[dir] += stage.get(id)?.[dir] ?? 0;
    return sum;
  }

  /** Download bytes count as the client consumes them. */
  addDownload(stage: TransferStage, id: string, bytes: number): void {
    this.#credit(stage, id, "down", bytes);
  }

  /** Upload bytes count from each receiver's highest count, whether or not it reached a boundary. A receiver first
      seen during a stage starts at its current count; a replacement starts at zero. */
  credit(id: string, receiver: { id: string; bytes: number }): void {
    const stage = this.#open?.record.stage;
    if (!stage || !Number.isSafeInteger(receiver.bytes) || receiver.bytes < 0) return;
    let ledger = this.#ledgers.get(id);
    if (!ledger) this.#ledgers.set(id, (ledger = new Map()));
    const maximum = ledger.get(receiver.id) ?? (ledger.size ? 0 : receiver.bytes);
    if (receiver.bytes > maximum) this.#credit(stage, id, "up", receiver.bytes - maximum);
    ledger.set(receiver.id, Math.max(maximum, receiver.bytes));
  }

  /** Adds a boundary; one missing any component is skipped, and the next valid one spans the gap. Returns the
      window since the previous boundary. */
  observe(boundary: Boundary, final = false): AggregateWindow | null {
    const open = this.#open;
    if (!open) return null;
    const { record } = open;
    const dirs = directions(record.stage);
    for (const id of record.participants) {
      const up = boundary.up[id];
      if (up) this.credit(id, up);
    }
    this.#latest = { boundary, final };
    const valid =
      record.participants.length > 0 &&
      record.participants.every((id) =>
        dirs.every((dir) =>
          dir === "down" ? Number.isFinite(boundary.down[id]) : !!boundary.up[id],
        ),
      );
    if (!valid) return null;
    if (!open.first || !open.last) {
      this.#start(open, boundary);
      return null;
    }
    const last = open.last;
    // A final boundary where any direction stood still ends the result at the previous one.
    const stood = (id: string, dir: Direction) =>
      dir === "down"
        ? boundary.down[id] === last.down[id]
        : boundary.up[id]!.id === last.up[id]!.id && boundary.up[id]!.bytes === last.up[id]!.bytes;
    if (final && record.participants.some((id) => dirs.some((dir) => stood(id, dir)))) return null;
    const continuous = record.participants.every((id) =>
      dirs.every((dir) => {
        if (dir === "down") return boundary.down[id]! >= last.down[id]!;
        const [a, b] = [last.up[id]!, boundary.up[id]!];
        return a.id === b.id && b.bytes >= a.bytes && b.nanos >= a.nanos;
      }),
    );
    // The same tick again, or an unchanged receiver clock, is stale: the receiver clock is authoritative.
    const unchanged =
      boundary.atMs <= last.atMs ||
      (dirs.includes("up") &&
        record.participants.some((id) => boundary.up[id]!.nanos === last.up[id]!.nanos));
    if (continuous && unchanged) return null;
    const sample = window(last, boundary, record);
    const full = window(open.first, boundary, record);
    if (!sample || !full) {
      // A replaced receiver or a regressed counter cannot be spanned.
      record.complete = false;
      record.endMs = boundary.atMs;
      this.begin(record.stage, record.participants, boundary.atMs, "evidence-resumed");
      return this.observe(boundary);
    }
    const ms = boundary.atMs - last.atMs;
    for (const dir of dirs) {
      const rate = rateOf(sample, dir)!;
      open.total[dir].observe((rate * ms) / 1000, ms);
      // A step no server moved is quiet on its evidence's clock: the client's, or the shortest receiver's.
      if (!rate) open.quiet[dir] += Math.min(...sample[dir]!.map((c) => c.durationMs));
      for (const component of sample[dir]!)
        open.servers.get(component.serverId)![dir].observe(component.bytes, component.durationMs);
    }
    open.combined.observe(
      (((sample.downBytesPerSec ?? 0) + (sample.upBytesPerSec ?? 0)) * ms) / 1000,
      ms,
    );
    open.last = boundary;
    const span = window(open.peakFrom!, boundary, record);
    if (span && shortestMs(span) >= PEAK_WINDOW_MS) {
      open.peakFrom = boundary;
      for (const dir of dirs) {
        raise(open.peaks, COMBINED, dir, rateOf(span, dir)!);
        for (const c of span[dir]!) raise(open.peaks, c.serverId, dir, c.bytesPerSec);
      }
    }
    full.quietMs = { ...open.quiet };
    record.full = full;
    record.endMs = boundary.atMs;
    const headline = open.stable && window(open.stable, boundary, record);
    if (headline)
      headline.quietMs = {
        down: open.quiet.down - open.stableQuiet.down,
        up: open.quiet.up - open.stableQuiet.up,
      };
    record.headline = open.stable ? headline : full;
    const mark = this.#mark(open, boundary);
    for (const dir of dirs)
      for (const component of sample[dir]!)
        if (component.bytes > 0) open.moved.get(component.serverId)![dir] = mark;
    return sample;
  }

  /** The early-finish score over the trailing four seconds of all directions together. */
  confidence() {
    return transferConfidence(this.#open?.combined.recent ?? []);
  }

  resetStability(): void {
    const open = this.#open;
    if (!open) return;
    open.combined = new RateSeries(SCORE_BUCKETS);
    open.wasStable = false;
    open.stable = null;
  }

  /** Follows the early-finish score; while it holds, the headline window starts where it began to hold. */
  trackStable(score: number): boolean {
    const open = this.#open;
    if (!open?.record.complete) return false;
    const stable = stillStable(open.wasStable, score);
    open.stable = stable ? (open.wasStable ? open.stable : open.last) : null;
    if (stable && !open.wasStable) {
      open.stableQuiet = { ...open.quiet };
      for (const dir of directions(open.record.stage)) open.total[dir].openWindow();
    }
    open.wasStable = stable;
    return stable;
  }

  /** Each direction's headline from the latest complete interval with enough evidence; `stable` prefers the early
      finish's steady window when it suffices for both, so a stage's figures describe one window. */
  settle(stage: TransferStage, stable: boolean): Record<Direction, Headline | null> {
    const intervals = this.intervals.filter((interval) => interval.stage === stage);
    const dirs = directions(stage);
    const reduce = (dir: Direction): Headline | null => {
      if (!dirs.includes(dir) || !intervals.at(-1)?.participants.length) return null;
      for (const record of intervals.toReversed()) {
        const open = this.#interval(record);
        if (!record.complete || !record.full || !open) continue;
        const steady = stable && dirs.every((each) => sufficient(record.headline, each));
        const window = steady ? record.headline! : record.full;
        const rate = rateOf(window, dir);
        if (!rate || !sufficient(window, dir)) continue;
        record.headline = window;
        return {
          window,
          result: {
            reportedBytesPerSec: rate,
            totalBytes: this.#stageTotal(stage, dir),
            peakBytesPerSec: Math.max(open.peaks.get(COMBINED)?.[dir] ?? 0, rate),
            // The steady window's moments opened where it began.
            stabilityPct: open.total[dir].stability(window !== record.full),
            quietMs: window.quietMs?.[dir] ?? 0,
          },
        };
      }
      const kept = this.#kept.get(`${stage}:${dir}`);
      return kept?.window
        ? {
            window: kept.window,
            result: {
              reportedBytesPerSec: kept.bytesPerSec,
              totalBytes: this.#stageTotal(stage, dir),
              peakBytesPerSec: kept.peakBytesPerSec,
              stabilityPct: kept.stabilityPct,
              quietMs: kept.window.quietMs?.[dir] ?? 0,
            },
          }
        : null;
    };
    return { down: reduce("down"), up: reduce("up") };
  }

  /** How long a stage's intervals span, from its first interval even past the interval limit. */
  coveredMs(stage: TransferStage): number {
    return coveredMs(this.intervals, stage, this.#evictedStart.get(stage));
  }

  /** One server's share: the latest complete interval where its own component has enough evidence. */
  serverResult(stage: TransferStage, dir: Direction, id: string): ThroughputResult | null {
    for (const record of this.intervals.toReversed()) {
      if (record.stage !== stage || !record.complete) continue;
      const component = record.full?.[dir]?.find((c) => c.serverId === id);
      const open = this.#interval(record);
      if (!component?.bytes || !open || component.durationMs < MIN_EVIDENCE_MS) continue;
      return {
        reportedBytesPerSec: component.bytesPerSec,
        totalBytes: this.#stageTotal(stage, dir, id),
        peakBytesPerSec: Math.max(open.peaks.get(id)?.[dir] ?? 0, component.bytesPerSec),
        stabilityPct: open.servers.get(id)![dir].stability(),
      };
    }
    const kept = this.#kept.get(`${stage}:${dir}:${id}`);
    return kept
      ? {
          reportedBytesPerSec: kept.bytesPerSec,
          totalBytes: this.#stageTotal(stage, dir, id),
          peakBytesPerSec: kept.peakBytesPerSec,
          stabilityPct: kept.stabilityPct,
        }
      : null;
  }

  /** Keeps what an interval the limit lets go measured, where nothing later measured it: the stage's full window and
      each server's share. Totals are read from the ledgers, which go on counting. */
  #keep(record: Interval): void {
    const open = this.#interval(record);
    if (!open || !record.complete || !record.full) return;
    const full = record.full;
    for (const dir of directions(record.stage)) {
      const rate = rateOf(full, dir);
      if (rate && sufficient(full, dir))
        this.#kept.set(`${record.stage}:${dir}`, {
          window: full,
          bytesPerSec: rate,
          peakBytesPerSec: Math.max(open.peaks.get(COMBINED)?.[dir] ?? 0, rate),
          stabilityPct: open.total[dir].stability(),
        });
      for (const component of full[dir] ?? [])
        if (component.bytes && component.durationMs >= MIN_EVIDENCE_MS)
          this.#kept.set(`${record.stage}:${dir}:${component.serverId}`, {
            window: null,
            bytesPerSec: component.bytesPerSec,
            peakBytesPerSec: Math.max(
              open.peaks.get(component.serverId)?.[dir] ?? 0,
              component.bytesPerSec,
            ),
            stabilityPct: open.servers.get(component.serverId)![dir].stability(),
          });
    }
  }

  #start(open: Open, boundary: Boundary): void {
    open.first = open.last = open.peakFrom = boundary;
    open.record.startMs = open.record.endMs = boundary.atMs;
    const start = this.#mark(open, boundary);
    for (const moved of open.moved.values())
      for (const dir of directions(open.record.stage)) moved[dir] = start;
  }

  #mark(open: Open, boundary: Boundary): Mark {
    const { record } = open;
    return {
      boundary,
      full: record.full,
      headline: record.headline,
      peaks: new Map([...open.peaks].map(([id, peak]) => [id, { ...peak }])),
      series: [open.total, ...open.servers.values()].flatMap((each) =>
        directions(record.stage).map((dir): [RateSeries, SeriesMark] => [
          each[dir],
          each[dir].mark,
        ]),
      ),
    };
  }

  #interval(record: Interval): Open | undefined {
    return this.#open?.record === record ? this.#open : this.#closed.get(record.id);
  }

  #credit(stage: TransferStage, id: string, dir: Direction, bytes: number): void {
    if (!(bytes > 0)) return;
    let totals = this.#stageTotals.get(stage);
    if (!totals) this.#stageTotals.set(stage, (totals = new Map()));
    const total = totals.get(id) ?? { down: 0, up: 0 };
    total[dir] += bytes;
    totals.set(id, total);
  }

  #stageTotal(stage: TransferStage, dir: Direction, id?: string): number {
    let sum = 0;
    for (const [server, totals] of this.#stageTotals.get(stage) ?? [])
      if (!id || server === id) sum += totals[dir];
    return sum;
  }
}

/** From a stage's first interval, or the start of the first one the log let go, to its last; gaps included. */
export function coveredMs(
  intervals: readonly Interval[],
  stage: TransferStage,
  firstStart?: number,
): number {
  const spans = intervals.filter((interval) => interval.stage === stage);
  return spans.length ? spans.at(-1)!.endMs - (firstStart ?? spans[0]!.startMs) : 0;
}

const shortestMs = (window: AggregateWindow) =>
  Math.min(window.endMs - window.startMs, ...(window.up ?? []).map((c) => c.durationMs));

function raise(peaks: Peaks, id: string, dir: Direction, rate: number): void {
  const peak = peaks.get(id) ?? {};
  peak[dir] = Math.max(peak[dir] ?? 0, rate);
  peaks.set(id, peak);
}

/** Whether a window spans the evidence floor on the client clock and on every receiver clock of `dir` (or both). */
export function sufficient(window: AggregateWindow | null, dir?: Direction): boolean {
  return (
    !!window &&
    window.endMs - window.startMs >= MIN_EVIDENCE_MS &&
    [
      ...(dir === "up" ? [] : (window.down ?? [])),
      ...(dir === "down" ? [] : (window.up ?? [])),
    ].every((c) => c.durationMs >= MIN_EVIDENCE_MS)
  );
}

function window(first: Boundary, last: Boundary, interval: Interval): AggregateWindow | null {
  if (last.atMs <= first.atMs) return null;
  const result: AggregateWindow = {
    startMs: first.atMs,
    endMs: last.atMs,
    down: null,
    up: null,
    downBytesPerSec: null,
    upBytesPerSec: null,
  };
  for (const dir of directions(interval.stage)) {
    const components: ComponentWindow[] = [];
    for (const serverId of interval.participants) {
      if (dir === "down") {
        const startBytes = first.down[serverId]!;
        const endBytes = last.down[serverId]!;
        const durationMs = last.atMs - first.atMs;
        if (!(endBytes >= startBytes)) return null;
        const bytes = endBytes - startBytes;
        components.push({
          serverId,
          startBytes,
          endBytes,
          bytes,
          durationMs,
          bytesPerSec: (bytes * 1000) / durationMs,
          clock: "client-monotonic",
        });
        continue;
      }
      const a = first.up[serverId];
      const b = last.up[serverId];
      if (!a || !b || a.id !== b.id || b.bytes < a.bytes || b.nanos <= a.nanos) return null;
      const durationMs = (b.nanos - a.nanos) / 1e6;
      components.push({
        serverId,
        startBytes: a.bytes,
        endBytes: b.bytes,
        bytes: b.bytes - a.bytes,
        durationMs,
        bytesPerSec: ((b.bytes - a.bytes) * 1000) / durationMs,
        clock: "receiver",
        startNanos: a.nanos,
        endNanos: b.nanos,
        startRequestMs: a.requestedAtMs,
        startResponseMs: a.receivedAtMs,
        endRequestMs: b.requestedAtMs,
        endResponseMs: b.receivedAtMs,
      });
    }
    const sum = components.reduce((total, c) => total + c.bytesPerSec, 0);
    if (dir === "down") [result.down, result.downBytesPerSec] = [components, sum];
    else [result.up, result.upBytesPerSec] = [components, sum];
  }
  return result;
}
