// The live rate a stage shows while it runs. Results never read it; the terminal clients share its vectors
// (api/liverate.testvectors.json), so its arithmetic stays exactly as it is.
import type { Checkpoint, Direction } from "./aggregate";

const MIN_WINDOW_MS = 800;
/** A rate shows once this much evidence follows the delivery that anchored it. */
const FIRST_MS = 500;
const FAST_WINDOW_MS = 750;
const REGIME_READY_MS = 2_000;
const DOWNSHIFT_CONFIRM_MS = 750;
const UPSHIFT_CONFIRM_MS = 500;

interface Span {
  start: number;
  end: number;
  bytes: number;
  /** Bytes up to and including this span. */
  total: number;
}

/** The growing window covers 85% of the current regime, never less than 800 ms. */
const windowMs = (regimeAgeMs: number) => {
  const age = Math.max(0, regimeAgeMs) || 0;
  return Math.min(age, Math.max(MIN_WINDOW_MS, age * 0.85));
};

/** A growing average that restarts on a confirmed 25% drop or 20% rise. The first delivery only anchors it: its
    bytes arrived over a span no window bounds (a socket buffer drained at once, a receiver's 64 KiB read), and on a
    slow link they alone would lift the first readings well above the rate. */
export class RateEstimator {
  /** Bytes per second to show; 0 until there is evidence. */
  presented = 0;
  #anchored = false;
  #spans: Span[] = [];
  #head = 0;
  #bytes = 0;
  #evidence = 0;
  #regimeStart = 0;
  #fast = 0;
  #candidate: { up: boolean; start: number; reference: number } | null = null;

  /** Adds `bytes` over `durationMs`; true when this confirmed a new regime. */
  observe(bytes: number, durationMs: number): boolean {
    if (!(durationMs > 0) || !Number.isFinite(durationMs)) return false;
    if (!this.#anchored) {
      this.#anchored = bytes > 0;
      return false;
    }
    const start = this.#evidence;
    this.#evidence += durationMs;
    bytes = Math.max(0, bytes) || 0;
    this.#bytes += bytes;
    this.#spans.push({ start, end: this.#evidence, bytes, total: this.#bytes });
    this.#recalculate();
    const changed = this.#regime(start);
    if (changed) this.#recalculate();
    const keep = Math.min(
      this.#evidence - windowMs(this.#evidence - this.#regimeStart),
      Math.max(this.#regimeStart, this.#evidence - FAST_WINDOW_MS),
      this.#candidate?.start ?? Infinity,
    );
    while (this.#head < this.#spans.length - 1 && this.#spans[this.#head]!.end <= keep) this.#head++;
    if (this.#head >= 1024 && this.#head * 2 >= this.#spans.length) {
      this.#spans = this.#spans.slice(this.#head);
      this.#head = 0;
    }
    return changed;
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

  #regime(start: number): boolean {
    const candidate = this.#candidate;
    if (!candidate) {
      if (this.#evidence - this.#regimeStart < REGIME_READY_MS || this.presented <= 0) return false;
      const ratio = this.#fast / this.presented;
      if (ratio < 0.75 || ratio > 1.2) this.#candidate = { up: ratio > 1.2, start, reference: this.presented };
      return false;
    }
    const ratio = candidate.reference > 0 ? this.#fast / candidate.reference : 1;
    if (candidate.up ? ratio < 1.1 : ratio > 0.85) {
      this.#candidate = null;
      return false;
    }
    if (this.#evidence - candidate.start < (candidate.up ? UPSHIFT_CONFIRM_MS : DOWNSHIFT_CONFIRM_MS)) return false;
    this.#regimeStart = candidate.start;
    this.#candidate = null;
    return true;
  }
}

const HINT_MAX_AGE_MS = 250;
const LANE_MAX_AGE_MS = 750;

interface ServerRates {
  down: RateEstimator;
  up: RateEstimator;
  downBytes: number;
  downAt: number;
  frame: Checkpoint | null;
  /** Page ms of the last five receiver records that grew. */
  arrivals: number[];
  /** Each upload lane's last POST rate and when it completed. */
  hints: Map<number, { rate: number; at: number }>;
}

/** Each server's presented rate, summed, so one lagging receiver never freezes the others. Page ms throughout. */
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

  /** Client-consumed bytes over the page time since the previous sample. */
  download(id: string, cumulative: number, now: number): void {
    const server = this.#servers.get(id);
    if (!server) return;
    server.down.observe(Math.max(0, cumulative - server.downBytes), now - server.downAt);
    server.downBytes = cumulative;
    server.downAt = now;
  }

  /** Receiver bytes over receiver time between consecutive records of one upload id. */
  receiver(id: string, frame: Checkpoint, now: number): void {
    const server = this.#servers.get(id);
    if (!server) return;
    const last = server.frame;
    server.frame = frame;
    if (!last || last.id !== frame.id || frame.nanos <= last.nanos || frame.bytes < last.bytes) return;
    server.up.observe(frame.bytes - last.bytes, (frame.nanos - last.nanos) / 1e6);
    if (frame.bytes === last.bytes) return;
    if (server.arrivals.push(now) > 5) server.arrivals.shift();
    server.hints.clear();
  }

  /** One locally timed POST completion. */
  hint(id: string, lane: number, bytes: number, elapsedMs: number, now: number): void {
    if (lane >= 0 && bytes > 0 && elapsedMs > 0)
      this.#servers.get(id)?.hints.set(lane, { rate: bytes / (elapsedMs / 1000), at: now });
  }

  /** Null until the direction's first bytes, so a new stage never presents zero. */
  rate(dir: Direction): number | null {
    let sum = 0;
    for (const server of this.#servers.values()) sum += server[dir].presented;
    if (sum > 0) this.#seen[dir] = true;
    return this.#seen[dir] ? sum : null;
  }

  /** While a receiver pauses irregularly, fresh hints from every lane bridge it within ±25%; null when no server
      needed it. `lanes` is a server's upload lane count. */
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
