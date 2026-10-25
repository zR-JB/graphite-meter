// A run's chart series in a fixed budget: the newest stretch as measured, older stretches folded into nested slots.
import type { Direction } from "../engine/types";
import { REVISION_MS, type LatencyBucket } from "../engine/measure/buckets";
import { add, type Moments } from "../engine/measure/stability";
import type { TransferStage } from "../engine/run/schedule";

export const SERIES_LIMIT = 1_200;
/** Between runs the idle monitor adds one bucket per probe. It keeps this many. */
export const IDLE_LIMIT = 60;

export interface ThroughputSample {
  t: number;
  bytesPerSec: number;
  /** Unique measured bytes of the run so far. */
  bytesCumulative: number;
  dir: Direction;
  stage: TransferStage;
  continuityId: number;
  /** The live ticks a sample stands for, from `t0` to `t1`; a folded one holds their mean time and rate. */
  ticks: number;
  t0: number;
  t1: number;
}

interface Kind<T> {
  revisableMs: number;
  group(item: T): string;
  lane(item: T): string;
  joins(last: T, next: T): boolean;
  at(item: T): number;
  start(item: T): number;
  end(item: T): number;
  merge(bin: T[], slot: { start: number; end: number }): T;
}

/** Items in time order within `limit`: the newest stretch whole, older items merged into nested slots of a fixed grid. */
class Folded<T> {
  readonly items: T[] = [];
  #grids = new Map<string, { origin: number; width: number }>();

  constructor(
    readonly kind: Kind<T>,
    readonly limit: number,
  ) {}

  clear(): void {
    this.items.length = 0;
    this.#grids.clear();
  }

  fit(): void {
    const { items, kind } = this;
    const done = new Set<string>();
    while (items.length > this.limit) {
      let newest = Number.NEGATIVE_INFINITY;
      for (const item of items) newest = Math.max(newest, kind.end(item));
      const horizon = newest - kind.revisableMs;
      const settled = new Map<string, number>();
      for (const item of items)
        if (kind.end(item) < horizon && !done.has(kind.group(item)))
          settled.set(kind.group(item), (settled.get(kind.group(item)) ?? 0) + 1);
      let group: string | null = null;
      for (const [name, count] of settled)
        if (count > 1 && (group === null || count > settled.get(group)!)) group = name;
      if (group === null) return;
      let grid = this.#grids.get(group);
      if (!grid) {
        let width = 1;
        for (const item of items)
          if (kind.group(item) === group)
            width = Math.max(width, kind.end(item) - kind.start(item));
        const first = items.find((item) => kind.group(item) === group)!;
        this.#grids.set(group, (grid = { origin: kind.start(first), width }));
      }
      const before = items.length;
      if (this.#fold(group, grid, horizon, true) < before) continue;
      if (grid.width <= horizon - grid.origin) grid.width *= 2;
      else if (this.#fold(group, grid, horizon, false) === before) done.add(group);
    }
  }

  #fold(
    group: string,
    { origin, width }: { origin: number; width: number },
    horizon: number,
    strict: boolean,
  ): number {
    const { items, kind } = this;
    const open = new Map<string, { index: number; slot: number; bin: T[] }>();
    const close = ({ index, slot, bin }: { index: number; slot: number; bin: T[] }) => {
      const start = origin + slot * width;
      if (bin.length > 1) items[index] = kind.merge(bin, { start, end: start + width });
    };
    let write = 0;
    for (const item of items) {
      if (kind.group(item) === group && kind.end(item) < horizon) {
        const slot = Math.floor((kind.at(item) - origin) / width);
        const lane = kind.lane(item);
        const entry = open.get(lane);
        if (entry?.slot === slot && (!strict || kind.joins(entry.bin.at(-1)!, item))) {
          entry.bin.push(item);
          continue;
        }
        if (entry) close(entry);
        open.set(lane, { index: write, slot, bin: [item] });
      }
      items[write++] = item;
    }
    for (const entry of open.values()) close(entry);
    items.length = write;
    return write;
  }
}

const latencyKind: Kind<LatencyBucket> = {
  revisableMs: REVISION_MS,
  group: (bucket) => bucket.phase,
  lane: () => "",
  // A reconnect's line break stays while its buckets are whole; folded history joins across it.
  joins: () => true,
  at: (bucket) => bucket.startT,
  start: (bucket) => bucket.startT,
  end: (bucket) => bucket.endT,
  merge: (bin, slot) => ({
    ...mergeLatency(bin),
    startT: Math.max(bin[0]!.startT, slot.start),
    endT: Math.min(bin.at(-1)!.endT, slot.end),
    continuityId: bin.at(-1)!.continuityId,
  }),
};

export class BucketSeries {
  readonly #folded: Folded<LatencyBucket>;

  constructor(limit = SERIES_LIMIT) {
    this.#folded = new Folded(latencyKind, limit);
  }

  get buckets(): readonly LatencyBucket[] {
    return this.#folded.items;
  }

  clear(): void {
    this.#folded.clear();
  }

  /** Inserts a bucket in start order, or replaces its earlier version on the same line. */
  upsert(bucket: LatencyBucket): void {
    const history = this.#folded.items;
    const same = (other: LatencyBucket) =>
      other.phase === bucket.phase && other.continuityId === bucket.continuityId;
    let at = history.length;
    while (at > 0 && history[at - 1]!.startT > bucket.startT) at--;
    let i = at - 1;
    while (i >= 0 && history[i]!.startT === bucket.startT && !same(history[i]!)) i--;
    if (i >= 0 && history[i]!.startT === bucket.startT) history[i] = bucket;
    else history.splice(at, 0, bucket);
    this.#folded.fit();
  }
}

export function idleBucket(t: number, rttMs: number, timedOut: boolean): LatencyBucket {
  const rtt = timedOut ? null : Math.max(0, rttMs);
  return {
    phase: "idle",
    continuityId: 0,
    startT: t,
    endT: t,
    medianRttMs: rtt,
    minRttMs: rtt,
    p95RttMs: rtt,
    maxRttMs: rtt,
    pingCount: 1,
    timeoutCount: timedOut ? 1 : 0,
  };
}

/** A merged bucket keeps the success-weighted median and the worst tail. */
function mergeLatency(bin: LatencyBucket[]): LatencyBucket {
  const replies = bin
    .filter((b) => b.medianRttMs !== null)
    .sort((a, b) => a.medianRttMs! - b.medianRttMs!);
  const successes = replies.reduce((sum, b) => sum + b.pingCount - b.timeoutCount, 0);
  let seen = 0;
  const at = replies.findIndex((b) => (seen += b.pingCount - b.timeoutCount) >= successes / 2);
  // An even split takes the midpoint, so folds nested in folds drift neither way.
  const upper = seen === successes / 2 ? replies[at + 1] : undefined;
  const median =
    at < 0 ? null : (replies[at]!.medianRttMs! + (upper ?? replies[at]!).medianRttMs!) / 2;
  const known = (values: (number | null)[]) => values.filter((value) => value !== null);
  const extreme = (pick: (...values: number[]) => number, values: (number | null)[]) =>
    known(values).length ? pick(...known(values)) : null;
  return {
    ...bin[0]!,
    endT: bin.at(-1)!.endT,
    medianRttMs: median,
    minRttMs: extreme(
      Math.min,
      bin.map((b) => b.minRttMs),
    ),
    p95RttMs: extreme(
      Math.max,
      bin.map((b) => b.p95RttMs),
    ),
    maxRttMs: extreme(
      Math.max,
      bin.map((b) => b.maxRttMs),
    ),
    pingCount: bin.reduce((sum, b) => sum + b.pingCount, 0),
    timeoutCount: bin.reduce((sum, b) => sum + b.timeoutCount, 0),
  };
}

const throughputKind: Kind<ThroughputSample> = {
  revisableMs: 0,
  group: (sample) => sample.stage,
  lane: (sample) => sample.dir,
  joins: (last, next) => last.continuityId === next.continuityId,
  at: (sample) => sample.t,
  start: (sample) => sample.t0,
  end: (sample) => sample.t1,
  merge: (bin) => {
    let ticks = 0;
    let t = 0;
    let rate = 0;
    for (const sample of bin) {
      ticks += sample.ticks;
      t += sample.t * sample.ticks;
      rate += sample.bytesPerSec * sample.ticks;
    }
    return {
      ...bin.at(-1)!,
      t: t / ticks,
      t0: bin[0]!.t0,
      bytesPerSec: rate / ticks,
      ticks,
    };
  },
};

/** A transfer stage's samples in constant size: its times, newest rates, and Peak and Stability over its whole seconds. */
export class StageTrace {
  readonly first: number;
  last: number;
  count = 0;
  readonly latest: Record<Direction, number> = { down: 0, up: 0 };
  #second = 0;
  #lanes: Record<Direction, { sum: number; n: number }> = {
    down: { sum: 0, n: 0 },
    up: { sum: 0, n: 0 },
  };
  #seconds: Moments = { count: 0, mean: 0, squares: 0 };
  #peak = 0;

  constructor(t: number) {
    this.first = this.last = t;
  }

  add(sample: ThroughputSample, replaced: ThroughputSample | null): void {
    const second = Math.floor((sample.t - this.first) / 1000);
    if (second > this.#second) {
      const rate = this.#current();
      if (rate !== null) {
        add(this.#seconds, rate);
        this.#peak = Math.max(this.#peak, rate);
      }
      for (const lane of Object.values(this.#lanes)) lane.sum = lane.n = 0;
      this.#second = second;
    }
    const lane = this.#lanes[sample.dir];
    if (replaced) {
      lane.sum -= replaced.bytesPerSec;
      lane.n--;
    } else this.count++;
    lane.sum += sample.bytesPerSec;
    lane.n++;
    this.last = sample.t;
    this.latest[sample.dir] = sample.bytesPerSec;
  }

  shape(): { peak: number; stability: number | null } | null {
    const rate = this.#current();
    const seconds = { ...this.#seconds };
    if (rate !== null) add(seconds, rate);
    if (!seconds.count) return null;
    const variation =
      seconds.mean > 0 ? Math.sqrt(seconds.squares / seconds.count) / seconds.mean : 1;
    return {
      peak: Math.max(this.#peak, rate ?? 0),
      stability: seconds.count >= 2 ? Math.max(0, 1 - variation) * 100 : null,
    };
  }

  #current(): number | null {
    const { down, up } = this.#lanes;
    if (!down.n && !up.n) return null;
    return (down.n ? down.sum / down.n : 0) + (up.n ? up.sum / up.n : 0);
  }
}

export class ThroughputSeries {
  readonly #folded = new Folded(throughputKind, SERIES_LIMIT);
  readonly traces: Partial<Record<TransferStage, StageTrace>> = {};

  get samples(): readonly ThroughputSample[] {
    return this.#folded.items;
  }

  clear(): void {
    this.#folded.clear();
    for (const stage of Object.keys(this.traces) as TransferStage[]) delete this.traces[stage];
  }

  /** Appends a sample, replacing one of the same line at the same time. */
  append(sample: ThroughputSample): void {
    const history = this.#folded.items;
    let replaced: ThroughputSample | null = null;
    // Only the equal-time tail can hold the same line's sample.
    for (let i = history.length - 1; i >= 0 && history[i]!.t === sample.t; i--) {
      const other = history[i]!;
      if (
        other.stage === sample.stage &&
        other.dir === sample.dir &&
        other.continuityId === sample.continuityId
      ) {
        replaced = other;
        history[i] = sample;
        break;
      }
    }
    if (!replaced) history.push(sample);
    (this.traces[sample.stage] ??= new StageTrace(sample.t)).add(sample, replaced);
    this.#folded.fit();
  }
}
