// One latency bus's probing, whatever carries it (docs/MEASUREMENTS.md#latency-probing): fixed or reply-driven pacing
// within an in-flight window, RFC 6298 deadlines fixed at send, outcomes batched for the owner, reconnects with
// backoff, and a drain to the probes' own deadlines at stop. One timer serves all of it, armed only for what is due.
import type { ProbeReport } from "../../protocol/messages";
import { ping, pong } from "../../protocol/wire";
import { DEADLINE_FLOOR_MS, RttEstimator } from "../measure/latency";
import type { ProbeOutcome } from "../types";

const FLUSH_MS = 50;
const BATCH_MAX = 128;
const RECONNECT_MIN_MS = 100;
const RECONNECT_MAX_MS = 2_000;
/** Evicted probes a late reply may still name: the only way the estimator learns a jump from fast to slow. */
const GRAVEYARD_MAX = 64;
const BACKUP_FLOOR_MS = 8;
const BACKUP_CEILING_MS = 1_000;

/** Time as the prober sees it, so tests can drive it. */
export interface ProberClock {
  now(): number;
  /** Epoch ms at `now() === 0`: outcomes leave the realm on epoch time. */
  readonly origin: number;
  /** Arms the one timer to call `wake()` in `ms`, replacing an armed one. */
  arm(ms: number): void;
  disarm(): void;
}

/** The connection side: a dial is answered by opened(), closed() or signIn(). */
export interface ProberIO {
  dial(): void;
  /** Sends on the open connection; throws or rejects when the browser could not send. */
  send(text: string): void | Promise<void>;
  /** Closes the current connection, which then answers nothing. */
  hangUp(): void;
  post(report: ProbeReport): void;
}

interface Probe {
  sentAt: number;
  deadline: number;
  /** A datagram's write resolves later; a probe that never left cannot time out. */
  confirmed: boolean;
  /** Warmup probes never enter a population. */
  measured: boolean;
}

type State = "connecting" | "open" | "draining" | "stopped";

export class Prober {
  #state: State = "connecting";
  readonly #pending = new Map<number, Probe>();
  readonly #graveyard = new Map<number, number>();
  readonly #rtt = new RttEstimator();
  /** RFC 6298's RTTVAR, which the reply-driven backup needs and the estimator keeps to its deadline. */
  #rttvar = 0;
  #nextId = 0;
  /** The latest probe: a reply-driven chain steps when it resolves. */
  #head = -1;
  #sentAt = -Infinity;
  #backupAt = Infinity;
  #outbox: ProbeOutcome[] = [];
  #flushAt = Infinity;
  #reconnectAt = Infinity;
  #backoff = 0;
  #stalled = false;
  /** This connection had a matched reply. */
  #replied = false;
  #measuring = false;
  /** When the stop arrived: outcomes observed later resolve their probes outside the window. */
  #stoppedAt = Infinity;
  #timerAt = Infinity;

  constructor(
    readonly options: { intervalMs: number | null; maxInFlight: number },
    readonly clock: ProberClock,
    readonly io: ProberIO,
  ) {}

  start(): void {
    this.io.dial();
  }

  opened(): void {
    if (this.#state !== "connecting") return;
    this.#state = "open";
    this.#backoff = 0;
    this.#replied = false;
    // Every connection probes at once.
    this.#sentAt = -Infinity;
    const now = this.clock.now();
    this.#step(now, true);
    this.#settle(now);
  }

  /** The connection was lost, or a dial failed. */
  closed(detail: string): void {
    if (this.#state === "stopped") return;
    if (this.#state === "open") this.#state = "connecting";
    const now = this.clock.now();
    // A deadline that passed before the gap stays a timeout; the others are unfinished, never timeouts.
    this.#sweep(now);
    this.#interrupt();
    if (this.#state === "draining") return this.#finish();
    // Once per outage, until replies flow again.
    if (!this.#stalled) this.io.post({ type: "stalled", detail });
    this.#stalled = true;
    this.#backoff = Math.min(RECONNECT_MAX_MS, this.#backoff * 2 || RECONNECT_MIN_MS);
    this.#reconnectAt = now + this.#backoff;
    this.#settle(now);
  }

  /** The server ended the login or grant: nothing pending gets a verdict. */
  signIn(): void {
    if (this.#state === "stopped") return;
    this.#interrupt();
    this.io.post({ type: "sign-in" });
    this.#finish();
  }

  /** A message from the server, read at `at` before it was parsed. */
  frame(text: string, at: number): void {
    if (this.#state !== "open" && this.#state !== "draining") return;
    const reply = pong(text);
    if (!reply) return;
    const probe = this.#pending.get(reply.id);
    if (!probe) {
      const sentAt = this.#graveyard.get(reply.id);
      if (sentAt === undefined) return;
      this.#graveyard.delete(reply.id);
      return this.#observe(at - sentAt);
    }
    this.#pending.delete(reply.id);
    if (!this.#replied) this.io.post({ type: "ready" });
    this.#replied = true;
    const rttMs = at - probe.sentAt;
    // Every reply keeps the deadline accurate, a warmup or late one too.
    this.#observe(rttMs);
    // A reply at or after its deadline cannot erase the timeout.
    const late = at >= probe.deadline;
    const handlingMs = reply.handlingNs !== null && reply.handlingNs / 1e6 <= rttMs ? reply.handlingNs / 1e6 : null;
    if (probe.measured) this.#record(probe, late ? probe.deadline : at, late, late ? null : handlingMs);
    if (!late && this.#stalled && this.#state === "open") {
      this.#stalled = false;
      this.io.post({ type: "resumed" });
    }
    this.#step(at, reply.id === this.#head);
    this.#settle(at);
  }

  /** Outcomes count from now on; a fixed cadence probes the window's start at once. */
  measure(): void {
    if (this.#state === "draining" || this.#state === "stopped") return;
    this.#measuring = true;
    if (this.options.intervalMs === null) return;
    this.#sentAt = -Infinity;
    const now = this.clock.now();
    this.#step(now, false);
    this.#settle(now);
  }

  /** A fixed cadence from the next send on, spaced from the last one; probes in flight keep their deadlines. */
  pace(intervalMs: number): void {
    this.options.intervalMs = intervalMs;
    this.wake();
  }

  /** Stops sending; measured probes in flight drain to their own deadlines, each at most 10 s away. Every stop is
      acknowledged. */
  stop(): void {
    if (this.#state === "stopped") return this.io.post({ type: "stopped" });
    if (this.#state === "draining") return;
    const now = this.clock.now();
    this.#stoppedAt = now;
    this.#reconnectAt = this.#backupAt = Infinity;
    for (const [id, probe] of this.#pending) if (!probe.measured) this.#pending.delete(id);
    // Nothing is in flight while disconnected.
    if (this.#state === "connecting") return this.#finish();
    this.#state = "draining";
    this.#settle(now);
  }

  wake(): void {
    this.#timerAt = Infinity;
    if (this.#state === "stopped") return;
    const now = this.clock.now();
    if (now >= this.#reconnectAt) {
      this.#reconnectAt = Infinity;
      this.io.dial();
    }
    this.#sweep(now);
    if (now >= this.#flushAt) this.#flush();
    if (this.options.intervalMs !== null) this.#step(now, false);
    else if (now >= this.#backupAt) this.#step(now, true);
    this.#settle(now);
  }

  /** Sends when it is the next probe's turn and the window has room: a fixed cadence keeps start-to-start spacing
      from the actual sends, never catching up; a reply-driven chain steps when its latest probe resolves or its
      backup fires, and sleeps while the window is full. */
  #step(now: number, chained: boolean): void {
    const { intervalMs, maxInFlight } = this.options;
    if (intervalMs === null) {
      if (!chained) return;
      this.#backupAt = Infinity;
    } else if (now < this.#sentAt + intervalMs) return;
    if (this.#state !== "open" || this.#pending.size >= maxInFlight) return;
    this.#send(now);
    if (intervalMs === null) this.#backupAt = now + this.#backupMs();
  }

  #send(now: number): void {
    const id = this.#nextId;
    // The window is tiny next to 2^32, so a wrapped id never matches a pending one.
    this.#nextId = (id + 1) >>> 0;
    this.#head = id;
    this.#sentAt = now;
    const probe: Probe = {
      sentAt: now,
      deadline: now + this.#rtt.deadlineMs(),
      confirmed: false,
      measured: this.#measuring,
    };
    this.#pending.set(id, probe);
    try {
      const sent = this.io.send(ping(id));
      if (!sent) probe.confirmed = true;
      else
        sent.then(
          () => this.#confirmed(id, probe),
          () => this.#failed(id, probe),
        );
    } catch {
      this.#failed(id, probe);
    }
  }

  #confirmed(id: number, probe: Probe): void {
    if (this.#pending.get(id) !== probe) return;
    probe.confirmed = true;
    this.#settle(this.clock.now());
  }

  /** A send the browser could not make is neither a timeout nor a step of the chain. */
  #failed(id: number, probe: Probe): void {
    if (this.#pending.get(id) !== probe) return;
    this.#pending.delete(id);
    if (probe.measured) {
      // The interruption follows the replies already observed, so no jitter pair spans it.
      this.#flush();
      this.io.post({ type: "interrupted", count: 1, reason: "send-failed" });
    }
    this.#settle(this.clock.now());
  }

  /** Resolves each confirmed probe against the deadline fixed when it was sent. */
  #sweep(now: number): void {
    let evicted = false;
    let head = false;
    for (const [id, probe] of this.#pending) {
      if (!probe.confirmed || now < probe.deadline) continue;
      this.#pending.delete(id);
      this.#graveyard.set(id, probe.sentAt);
      if (this.#graveyard.size > GRAVEYARD_MAX) this.#graveyard.delete(this.#graveyard.keys().next().value!);
      if (probe.measured) this.#record(probe, probe.deadline, true, null);
      evicted = true;
      head ||= id === this.#head;
    }
    // A timeout completes a chain step as a reply would.
    if (evicted) this.#step(now, head);
  }

  #observe(rttMs: number): void {
    const srtt = this.#rtt.srttMs;
    this.#rttvar = srtt === null ? rttMs / 2 : 0.75 * this.#rttvar + 0.25 * Math.abs(srtt - rttMs);
    this.#rtt.observe(rttMs);
  }

  /** The RTO within 8 ms and 1 s, so a reply-driven chain covers a late reply well before its deadline. */
  #backupMs(): number {
    const srtt = this.#rtt.srttMs;
    if (srtt === null) return DEADLINE_FLOOR_MS;
    return Math.min(BACKUP_CEILING_MS, Math.max(BACKUP_FLOOR_MS, srtt + 4 * Math.max(1, this.#rttvar)));
  }

  #record(probe: Probe, observedAt: number, timedOut: boolean, handlingMs: number | null): void {
    this.#outbox.push({
      rttMs: observedAt - probe.sentAt,
      timedOut,
      handlingMs,
      observedAt: this.clock.origin + observedAt,
      inWindow: observedAt <= this.#stoppedAt,
    });
    if (this.#outbox.length >= BATCH_MAX) this.#flush();
    else if (this.#flushAt === Infinity) this.#flushAt = this.clock.now() + FLUSH_MS;
  }

  #flush(): void {
    this.#flushAt = Infinity;
    if (!this.#outbox.length) return;
    this.io.post({ type: "outcomes", outcomes: this.#outbox });
    this.#outbox = [];
  }

  /** Measured probes in flight lose their verdict. */
  #interrupt(): void {
    let count = 0;
    for (const probe of this.#pending.values()) if (probe.measured) count++;
    this.#pending.clear();
    this.#flush();
    if (count) this.io.post({ type: "interrupted", count, reason: "unresolved" });
  }

  #finish(): void {
    this.#state = "stopped";
    this.#timerAt = Infinity;
    this.clock.disarm();
    this.#flush();
    this.io.post({ type: "stopped" });
    this.io.hangUp();
  }

  /** Ends a drain once nothing is pending, or at the last deadline, where a write still unconfirmed stays
      unresolved; otherwise arms the timer for the earliest of the next send, a confirmed deadline, the batch flush,
      the drain's end and a reconnect. An armed timer that fires earlier just re-arms. */
  #settle(now: number): void {
    if (this.#state === "stopped") return;
    let at = Math.min(this.#flushAt, this.#reconnectAt);
    let last = -Infinity;
    for (const probe of this.#pending.values()) {
      if (probe.confirmed) at = Math.min(at, probe.deadline);
      last = Math.max(last, probe.deadline);
    }
    if (this.#state === "draining") {
      if (now < last) at = Math.min(at, last);
      else {
        this.#sweep(now);
        this.#interrupt();
        return this.#finish();
      }
    }
    const { intervalMs, maxInFlight } = this.options;
    // A backup due while the window is full puts the chain to sleep; a full cadence waits for a free slot.
    if (this.#state === "open")
      at = Math.min(
        at,
        intervalMs === null ? this.#backupAt : this.#pending.size < maxInFlight ? this.#sentAt + intervalMs : Infinity,
      );
    if (at >= this.#timerAt) return;
    this.#timerAt = at;
    this.clock.arm(Math.max(0, at - now));
  }
}
