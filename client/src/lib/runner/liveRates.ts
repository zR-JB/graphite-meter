// Live presentation rates; results never read them.
import type { FlowDirection, ReceiverCheckpoint } from "./contract";

export const PRESENTATION_MIN_WINDOW_MS = 800;
const FAST_WINDOW_MS = 750;
const REGIME_READY_MS = 2_000;
export const REGIME_DOWNSHIFT_CONFIRM_MS = 750;
export const REGIME_UPSHIFT_CONFIRM_MS = 500;
export const STALL_PRESENTATION_MS = 800;
const HINT_MAX_AGE_MS = 250;
const LANE_MAX_AGE_MS = 750;

interface Span {
  start: number;
  end: number;
  bytes: number;
}

/** The growing window covers 85% of the current regime, never less than 800 ms. */
export function presentationWindowMs(regimeAgeMs: number): number {
  const age = Math.max(0, regimeAgeMs) || 0;
  return Math.min(age, Math.max(PRESENTATION_MIN_WINDOW_MS, age * 0.85));
}

function intervalRate(
  spans: readonly Span[],
  start: number,
  end: number,
): number {
  let bytes = 0;
  let ms = 0;
  for (const span of spans) {
    const overlap = Math.min(end, span.end) - Math.max(start, span.start);
    if (overlap <= 0) continue;
    bytes += (span.bytes * overlap) / (span.end - span.start);
    ms += overlap;
  }
  return ms > 0 ? (bytes * 1_000) / ms : 0;
}

/** A regime-restarting growing average: a confirmed 25% drop or 20% rise starts a new window. */
export class GrowingRateEstimator {
  #spans: Span[] = [];
  #evidence = 0;
  #regimeStart = 0;
  #candidate: { up: boolean; start: number; reference: number } | null = null;
  presented = 0;
  #fast = 0;

  reset(): void {
    this.#spans = [];
    this.#evidence = this.#regimeStart = this.presented = this.#fast = 0;
    this.#candidate = null;
  }

  /** True when this observation confirmed a new regime. */
  observe(bytes: number, durationMs: number): boolean {
    if (!(durationMs > 0) || !Number.isFinite(durationMs)) return false;
    const start = this.#evidence;
    this.#evidence += durationMs;
    this.#spans.push({
      start,
      end: this.#evidence,
      bytes: Math.max(0, bytes) || 0,
    });
    this.#recalculate();
    const changed = this.#regime(start);
    if (changed) this.#recalculate();
    const keep = Math.min(
      this.#evidence - presentationWindowMs(this.#evidence - this.#regimeStart),
      Math.max(this.#regimeStart, this.#evidence - FAST_WINDOW_MS),
      this.#candidate?.start ?? Infinity,
    );
    while (this.#spans.length > 1 && this.#spans[0].end <= keep)
      this.#spans.shift();
    return changed;
  }

  #recalculate(): void {
    const window = presentationWindowMs(this.#evidence - this.#regimeStart);
    this.presented = intervalRate(
      this.#spans,
      this.#evidence - window,
      this.#evidence,
    );
    const fastStart = Math.max(
      this.#regimeStart,
      this.#evidence - FAST_WINDOW_MS,
    );
    this.#fast = intervalRate(this.#spans, fastStart, this.#evidence);
  }

  #regime(start: number): boolean {
    const candidate = this.#candidate;
    if (!candidate) {
      if (
        this.#evidence - this.#regimeStart < REGIME_READY_MS ||
        this.presented <= 0
      )
        return false;
      const ratio = this.#fast / this.presented;
      if (ratio < 0.75 || ratio > 1.2)
        this.#candidate = { up: ratio > 1.2, start, reference: this.presented };
      return false;
    }
    const ratio =
      candidate.reference > 0 ? this.#fast / candidate.reference : 1;
    if (candidate.up ? ratio < 1.1 : ratio > 0.85) {
      this.#candidate = null;
      return false;
    }
    const confirmMs = candidate.up
      ? REGIME_UPSHIFT_CONFIRM_MS
      : REGIME_DOWNSHIFT_CONFIRM_MS;
    if (this.#evidence - candidate.start < confirmMs) return false;
    this.#regimeStart = candidate.start;
    this.#candidate = null;
    return true;
  }
}

/** A stalled presentation falls to zero over 800 ms. */
export const stallRate = (from: number, elapsedMs: number): number =>
  Math.max(0, from || 0) *
  (1 - Math.min(1, Math.max(0, elapsedMs / STALL_PRESENTATION_MS)));

interface ServerRates {
  down: GrowingRateEstimator;
  up: GrowingRateEstimator;
  downBytes: number;
  downAt: number;
  frame: ReceiverCheckpoint | null;
  arrivals: number[];
  hints: Map<number, { rate: number; at: number }>;
}

/** Per-server presentation summed across servers, so one lagging receiver never freezes the others. */
export class LiveRates {
  #servers = new Map<string, ServerRates>();

  /** Starts every series again from each server's current download count. */
  reset(counts: Record<string, number>, now = performance.now()): void {
    this.#servers.clear();
    for (const [id, downBytes] of Object.entries(counts))
      this.restart(id, downBytes, now);
  }

  /** One server's presentation starts again; a stalled server contributes nothing. */
  restart(id: string, downBytes: number, now = performance.now()): void {
    this.#servers.set(id, {
      down: new GrowingRateEstimator(),
      up: new GrowingRateEstimator(),
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

  /** Client-consumed bytes over the wall time since the previous tick. */
  download(id: string, cumulative: number, now: number): void {
    const server = this.#servers.get(id);
    if (!server) return;
    server.down.observe(
      Math.max(0, cumulative - server.downBytes),
      now - server.downAt,
    );
    server.downBytes = cumulative;
    server.downAt = now;
  }

  /** Receiver bytes over receiver time between consecutive records of one receiver. */
  receiver(id: string, frame: ReceiverCheckpoint, now: number): void {
    const server = this.#servers.get(id);
    if (!server) return;
    const last = server.frame;
    server.frame = frame;
    if (
      !last ||
      last.id !== frame.id ||
      frame.nanos <= last.nanos ||
      frame.bytes < last.bytes
    )
      return;
    server.up.observe(
      frame.bytes - last.bytes,
      (frame.nanos - last.nanos) / 1e6,
    );
    if (frame.bytes > last.bytes) {
      server.arrivals = [...server.arrivals.slice(-4), now];
      server.hints.clear();
    }
  }

  /** One locally timed POST completion; presentation evidence only. */
  hint(
    id: string,
    lane: number,
    bytes: number,
    elapsedMs: number,
    now: number,
  ): void {
    if (lane >= 0 && bytes > 0 && elapsedMs > 0)
      this.#servers
        .get(id)
        ?.hints.set(lane, { rate: bytes / (elapsedMs / 1000), at: now });
  }

  rate(dir: FlowDirection): number {
    let sum = 0;
    for (const server of this.#servers.values()) sum += server[dir].presented;
    return sum;
  }

  /** While a receiver pauses irregularly, fresh hints from every lane bridge it within ±25%. */
  bridgedUpload(now: number, lanes: (id: string) => number): number | null {
    let total = 0;
    let bridged = false;
    for (const [id, server] of this.#servers) {
      const authority = server.up.presented;
      total += authority;
      const gaps = server.arrivals
        .slice(1)
        .map((at, i) => at - server.arrivals[i])
        .sort((a, b) => a - b);
      const last = server.arrivals.at(-1) ?? now;
      const hints = [...server.hints.values()].filter(
        (hint) => now - hint.at <= LANE_MAX_AGE_MS,
      );
      if (
        gaps.length < 3 ||
        authority <= 0 ||
        now - last <= Math.max(300, 3 * gaps[Math.floor(gaps.length / 2)]) ||
        hints.length !== lanes(id) ||
        now - Math.max(...hints.map((hint) => hint.at)) > HINT_MAX_AGE_MS
      )
        continue;
      const estimate = hints.reduce((sum, hint) => sum + hint.rate, 0);
      total +=
        Math.min(1.25 * authority, Math.max(0.75 * authority, estimate)) -
        authority;
      bridged = true;
    }
    return bridged ? total : null;
  }
}
