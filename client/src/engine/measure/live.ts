// The live rate a running stage shows; the terminal client shares its vectors (api/liverate.testvectors.json).
import type { Checkpoint, Direction } from "../types";

const MIN_WINDOW_MS = 800;
/** A rate shows once this much evidence follows the delivery that anchored it. */
const FIRST_MS = 500;
const FAST_WINDOW_MS = 750;
const REGIME_READY_MS = 2_000;
const DOWNSHIFT_CONFIRM_MS = 750;
const UPSHIFT_CONFIRM_MS = 500;
/** A window keeps this many spans before its older ones merge. The shared vectors stay below it. */
const MAX_SPANS = 4_096;

interface Span {
  start: number;
  end: number;
  bytes: number;
  total: number;
}

const departs = (ratio: number) => ratio < 0.75 || ratio > 1.2;

/** The growing window covers 85% of the current regime, never less than 800 ms. */
const windowMs = (regimeAgeMs: number) => {
  const age = Math.max(0, regimeAgeMs) || 0;
  return Math.min(age, Math.max(MIN_WINDOW_MS, age * 0.85));
};

/** A confirmed regime change: how much evidence ago the live rate first read it, and the rate it read before. */
export interface Shift {
  sinceMs: number;
  fromBytesPerSec: number;
}

/** Grows an average and restarts it on a confirmed 25% drop or 20% rise; the first delivery, a drained buffer, only anchors it. */
export class RateEstimator {
  /** Holds the bytes per second to show, 0 until there is evidence. */
  presented = 0;
  #anchored = false;
  #spans: Span[] = [];
  #head = 0;
  #bytes = 0;
  #evidence = 0;
  #regimeStart = 0;
  #fast = 0;
  #candidate: { up: boolean; start: number; reference: number; before: number } | null = null;
  /** A regime holds once a readiness check finds it steady and, in single precision, not rising; changes count between held ones. */
  #held = false;
  #change: { start: number; before: number } | null = null;

  /** Adds `bytes` over `durationMs` and returns the change whose regime this found holding, if any. */
  observe(bytes: number, durationMs: number): Shift | null {
    if (!(durationMs > 0) || !Number.isFinite(durationMs)) return null;
    if (!this.#anchored) {
      this.#anchored = bytes > 0;
      return null;
    }
    const start = this.#evidence;
    this.#evidence += durationMs;
    bytes = Math.max(0, bytes) || 0;
    this.#bytes += bytes;
    this.#spans.push({ start, end: this.#evidence, bytes, total: this.#bytes });
    this.#recalculate();
    const regime = this.#regimeStart;
    const shift = this.#regime(start);
    if (this.#regimeStart !== regime) this.#recalculate();
    const keep = Math.min(
      this.#evidence - windowMs(this.#evidence - this.#regimeStart),
      Math.max(this.#regimeStart, this.#evidence - FAST_WINDOW_MS),
      this.#candidate?.start ?? Infinity,
    );
    while (this.#head < this.#spans.length - 1 && this.#spans[this.#head]!.end <= keep)
      this.#head++;
    if (this.#head >= 1024 && this.#head * 2 >= this.#spans.length) {
      this.#spans = this.#spans.slice(this.#head);
      this.#head = 0;
    }
    if (this.#spans.length - this.#head > MAX_SPANS) this.#coarsen();
    return shift;
  }

  /** Merges older spans in pairs, all but the fast window and a pending regime change, so memory stays bounded. */
  #coarsen(): void {
    const fine = Math.min(this.#evidence - 2 * FAST_WINDOW_MS, this.#candidate?.start ?? Infinity);
    const spans = this.#spans;
    const merged: Span[] = [];
    let i = this.#head;
    for (; i + 1 < spans.length && spans[i + 1]!.end <= fine; i += 2)
      merged.push({
        start: spans[i]!.start,
        end: spans[i + 1]!.end,
        bytes: spans[i]!.bytes + spans[i + 1]!.bytes,
        total: spans[i + 1]!.total,
      });
    this.#spans = merged.concat(spans.slice(i));
    this.#head = 0;
  }

  #recalculate(): void {
    const window = windowMs(this.#evidence - this.#regimeStart);
    this.presented = this.#evidence < FIRST_MS ? 0 : this.#rateSince(this.#evidence - window);
    this.#fast = this.#rateSince(Math.max(this.#regimeStart, this.#evidence - FAST_WINDOW_MS));
  }

  /** Prefix totals find a fractional window edge without rescanning a long stage. */
  #rateSince(start: number): number {
    let lo = this.#head;
    let hi = this.#spans.length - 1;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (this.#spans[mid]!.end <= start) lo = mid + 1;
      else hi = mid;
    }
    const first = this.#spans[lo]!;
    const from = Math.max(start, first.start);
    const ms = this.#evidence - from;
    if (!(ms > 0)) return 0;
    const partial = first.bytes * ((first.end - from) / (first.end - first.start));
    return ((this.#bytes - first.total + partial) * 1000) / ms;
  }

  #regime(start: number): Shift | null {
    const candidate = this.#candidate;
    if (!candidate) {
      if (this.#evidence - this.#regimeStart < REGIME_READY_MS || this.presented <= 0) return null;
      const ratio = this.#fast / this.presented;
      if (!departs(ratio)) return Math.fround(ratio) > 1 ? null : this.#hold();
      // What the window read before the fast window that holds the change.
      const window = windowMs(this.#evidence - this.#regimeStart);
      const before =
        (this.presented * window - this.#fast * FAST_WINDOW_MS) / (window - FAST_WINDOW_MS);
      this.#candidate = { up: ratio > 1.2, start, reference: this.presented, before };
      return null;
    }
    const ratio = candidate.reference > 0 ? this.#fast / candidate.reference : 1;
    if (candidate.up ? ratio < 1.1 : ratio > 0.85) {
      this.#candidate = null;
      return null;
    }
    if (
      this.#evidence - candidate.start <
      (candidate.up ? UPSHIFT_CONFIRM_MS : DOWNSHIFT_CONFIRM_MS)
    )
      return null;
    this.#regimeStart = candidate.start;
    this.#candidate = null;
    if (this.#held) this.#change = { start: candidate.start, before: candidate.before };
    this.#held = false;
    return null;
  }

  #hold(): Shift | null {
    const change = this.#change;
    this.#held = true;
    this.#change = null;
    return change && departs(this.presented / change.before)
      ? { sinceMs: this.#evidence - change.start, fromBytesPerSec: change.before }
      : null;
  }
}

/** A speed change the live rate confirmed: when it first read it, the rate it read before, and the one it confirmed. */
export interface SpeedChange {
  atMs: number;
  fromBytesPerSec: number;
  toBytesPerSec: number;
}

const changeOf = (shift: Shift | null, rate: RateEstimator, now: number): SpeedChange | null =>
  shift && {
    atMs: now - shift.sinceMs,
    fromBytesPerSec: shift.fromBytesPerSec,
    toBytesPerSec: rate.presented,
  };

const HINT_MAX_AGE_MS = 250;
const LANE_MAX_AGE_MS = 750;

interface ServerRates {
  down: RateEstimator;
  up: RateEstimator;
  downBytes: number;
  downAt: number;
  frame: Checkpoint | null;
  /** Records the page ms of the last five receiver records that grew. */
  arrivals: number[];
  /** Maps each upload lane to its last POST rate and when it completed. */
  hints: Map<number, { rate: number; at: number }>;
}

/** Sums each server's presented rate, so one lagging receiver never freezes the others. Page ms throughout. */
export class LiveRates {
  #servers = new Map<string, ServerRates>();
  #seen: Record<Direction, boolean> = { down: false, up: false };

  reset(counts: ReadonlyMap<string, number>, now: number): void {
    this.#servers.clear();
    this.#seen = { down: false, up: false };
    for (const [id, downBytes] of counts) this.restart(id, downBytes, now);
  }

  /** One server starts over, as when it begins or ends recovering. */
  restart(id: string, downBytes: number, now: number): void {
    this.#servers.set(id, {
      down: new RateEstimator(),
      up: new RateEstimator(),
      downBytes,
      downAt: now,
      frame: null,
      arrivals: [],
      hints: new Map(),
    });
  }

  drop(id: string): void {
    this.#servers.delete(id);
  }

  /** Returns the speed change this confirmed, if any; every change is timed in page ms. */
  download(id: string, cumulative: number, now: number): SpeedChange | null {
    const server = this.#servers.get(id);
    if (!server) return null;
    const shift = server.down.observe(
      Math.max(0, cumulative - server.downBytes),
      now - server.downAt,
    );
    server.downBytes = cumulative;
    server.downAt = now;
    return changeOf(shift, server.down, now);
  }

  receiver(id: string, frame: Checkpoint, now: number): SpeedChange | null {
    const server = this.#servers.get(id);
    if (!server) return null;
    const last = server.frame;
    server.frame = frame;
    if (!last || last.id !== frame.id || frame.nanos <= last.nanos || frame.bytes < last.bytes)
      return null;
    const shift = server.up.observe(frame.bytes - last.bytes, (frame.nanos - last.nanos) / 1e6);
    if (frame.bytes > last.bytes) {
      if (server.arrivals.push(now) > 5) server.arrivals.shift();
      server.hints.clear();
    }
    return changeOf(shift, server.up, now);
  }

  hint(id: string, lane: number, bytes: number, elapsedMs: number, now: number): void {
    if (lane >= 0 && bytes > 0 && elapsedMs > 0)
      this.#servers.get(id)?.hints.set(lane, { rate: bytes / (elapsedMs / 1000), at: now });
  }

  /** Returns null until the direction's first bytes, so a new stage never presents zero. */
  rate(dir: Direction): number | null {
    let sum = 0;
    for (const server of this.#servers.values()) sum += server[dir].presented;
    if (sum > 0) this.#seen[dir] = true;
    return this.#seen[dir] ? sum : null;
  }

  /** While a receiver pauses irregularly, fresh hints from every lane bridge it within ±25%; null when no server needed it. */
  bridgedUpload(now: number, lanes: (id: string) => number): number | null {
    let total = 0;
    let bridged = false;
    for (const [id, server] of this.#servers) {
      const authority = server.up.presented;
      total += authority;
      const { arrivals } = server;
      if (arrivals.length < 4 || authority <= 0) continue;
      const gaps = arrivals.slice(1).map((at, i) => at - arrivals[i]!);
      const median = gaps.sort((a, b) => a - b)[gaps.length >> 1]!;
      const hints = [...server.hints.values()].filter((hint) => now - hint.at <= LANE_MAX_AGE_MS);
      if (
        now - arrivals.at(-1)! <= Math.max(300, 3 * median) ||
        hints.length !== lanes(id) ||
        now - Math.max(...hints.map((hint) => hint.at)) > HINT_MAX_AGE_MS
      )
        continue;
      const estimate = hints.reduce((sum, hint) => sum + hint.rate, 0);
      total += Math.min(1.25 * authority, Math.max(0.75 * authority, estimate)) - authority;
      bridged = true;
    }
    return bridged ? total : null;
  }
}
