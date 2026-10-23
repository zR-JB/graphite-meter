// One latency bus's probing, whatever carries it (docs/MEASUREMENTS.md#latency-probing), all on one timer.
import type { ProbeReport } from "../../protocol/messages";
import { ping, pong } from "../../protocol/wire";
import { RttEstimator } from "../measure/latency";
import { GAP_MS } from "../run/clock";
import type { ProbeOutcome } from "../types";

const FLUSH_MS = 50;
const BATCH_MAX = 128;
/** Evicted probes a late reply may still name: the only way the estimator learns a jump from fast to slow. */
const GRAVEYARD_MAX = 64;
const BACKUP_FLOOR_MS = 8;
const BACKUP_CEILING_MS = 1_000;

interface ProberClock {
  now(): number;
  /** Epoch ms at `now() === 0`: outcomes leave the realm on epoch time. */
  readonly origin: number;
  /** Arms the one timer to call `wake()` in `ms`, replacing an armed one. */
  arm(ms: number): void;
  disarm(): void;
}

/** The connection side: a dial is answered by opened(), closed() or signIn(), and may lapse() first; only the owner dials again. */
interface ProberIO {
  dial(): void;
  /** Sends on the open connection; throws or rejects when the browser could not send. */
  send(text: string): void | Promise<void>;
  hangUp(): void;
  post(report: ProbeReport): void;
}

interface Probe {
  sentAt: number;
  deadline: number;
  /** A datagram's write resolves later; one still unconfirmed at its deadline never left, so it cannot time out. */
  confirmed: boolean;
  /** Warmup probes never enter a population. */
  measured: boolean;
  /** When its window ended: a later outcome resolves it outside the window. */
  cutoff: number;
}

type State = "connecting" | "open" | "stopped";

export class Prober {
  #state: State = "connecting";
  readonly #pending = new Map<number, Probe>();
  readonly #graveyard = new Map<number, number>();
  readonly #rtt = new RttEstimator();
  #pace: { intervalMs: number | null; maxInFlight: number };
  #nextId = 0;
  /** The latest probe: a reply-driven chain steps when it resolves. */
  #head = -1;
  #sentAt = -Infinity;
  #backupAt = Infinity;
  #outbox: ProbeOutcome[] = [];
  #flushAt = Infinity;
  #stalled = false;
  #replied = false;
  #measuring = false;
  /** A window ended and its probes in flight have yet to resolve. */
  #draining = false;
  #timerAt = Infinity;

  constructor(
    { intervalMs, maxInFlight }: { intervalMs: number | null; maxInFlight: number },
    readonly clock: ProberClock,
    readonly io: ProberIO,
  ) {
    this.#pace = { intervalMs, maxInFlight };
  }

  start(): void {
    this.io.dial();
  }

  opened(): void {
    if (this.#state !== "connecting") return;
    this.#state = "open";
    this.#replied = false;
    // Every connection probes at once.
    this.#sentAt = -Infinity;
    const now = this.clock.now();
    this.#step(now, true);
    this.#settle(now);
  }

  closed(detail: string): void {
    if (this.#state === "stopped") return;
    if (this.#state === "open") this.#state = "connecting";
    const now = this.clock.now();
    // A deadline that passed before the gap stays a timeout; the others are unfinished, never timeouts.
    this.#sweep(now);
    this.#interrupt();
    // Once per outage, until replies flow again.
    if (!this.#stalled) this.io.post({ type: "stalled", detail });
    this.#stalled = true;
    this.#settle(now);
  }

  /** A dial outlived its budget and still awaits the browser's verdict: an outage as far as the owner can tell. */
  lapsed(detail: string): void {
    if (this.#state !== "connecting" || this.#stalled) return;
    this.#stalled = true;
    this.io.post({ type: "stalled", detail });
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
    if (this.#state !== "open") return;
    // A timer this late means the worker or its page froze; a reply that reached it is read only after the freeze.
    if (at - this.#timerAt > GAP_MS) this.wake();
    const reply = pong(text);
    if (!reply) return;
    const probe = this.#pending.get(reply.id);
    if (!probe) {
      const sentAt = this.#graveyard.get(reply.id);
      if (sentAt === undefined) return;
      this.#graveyard.delete(reply.id);
      return this.#rtt.observe(at - sentAt);
    }
    this.#pending.delete(reply.id);
    if (!this.#replied) this.io.post({ type: "ready" });
    this.#replied = true;
    const rttMs = at - probe.sentAt;
    // Every reply keeps the deadline accurate, a warmup or late one too.
    this.#rtt.observe(rttMs);
    // A reply at or after its deadline cannot erase the timeout.
    const late = at >= probe.deadline;
    const handlingMs =
      reply.handlingNs !== null && reply.handlingNs / 1e6 <= rttMs ? reply.handlingNs / 1e6 : null;
    if (probe.measured)
      this.#record(probe, late ? probe.deadline : at, late, late ? null : handlingMs);
    if (!late && this.#stalled) {
      this.#stalled = false;
      this.io.post({ type: "resumed" });
    }
    this.#step(at, reply.id === this.#head);
    this.#settle(at);
  }

  /** Outcomes count from now on; a fixed cadence probes the window's start at once. */
  measure(): void {
    if (this.#state === "stopped") return;
    this.#measuring = true;
    if (this.#pace.intervalMs === null) return;
    this.#sentAt = -Infinity;
    const now = this.clock.now();
    this.#step(now, false);
    this.#settle(now);
  }

  /** From the next send on: a fixed cadence spaced from the last send, or a reply-driven chain at once. */
  pace(intervalMs: number | null, maxInFlight = this.#pace.maxInFlight): void {
    this.#pace = { intervalMs, maxInFlight };
    this.#backupAt = intervalMs === null ? this.clock.now() : Infinity;
    this.wake();
  }

  /** Ends the measured window: its probes drain to their deadlines, at most 10 s away; the bus probes on, unmeasured. */
  stop(): void {
    if (this.#state === "stopped") return this.io.post({ type: "drained" });
    const now = this.clock.now();
    this.#measuring = false;
    this.#draining = true;
    for (const probe of this.#pending.values())
      if (probe.measured) probe.cutoff = Math.min(probe.cutoff, now);
    this.#settle(now);
  }

  wake(): void {
    const due = this.#timerAt;
    this.#timerAt = Infinity;
    if (this.#state === "stopped") return;
    const now = this.clock.now();
    // Probes in flight across a freeze get no verdict: their replies may wait unread, and their RTTs carry the freeze.
    if (now - due > GAP_MS) this.#interrupt();
    this.#sweep(now);
    if (now >= this.#flushAt) this.#flush();
    if (this.#pace.intervalMs !== null) this.#step(now, false);
    else if (now >= this.#backupAt) this.#step(now, true);
    this.#settle(now);
  }

  /** Sends when it is the next probe's turn and the window has room; a fixed cadence never catches up on late sends. */
  #step(now: number, chained: boolean): void {
    const { intervalMs, maxInFlight } = this.#pace;
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
      cutoff: Infinity,
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

  /** Resolves each probe at its deadline: a timeout, or a send failure while its write is still unconfirmed. */
  #sweep(now: number): void {
    let evicted = false;
    let head = false;
    let unsent = 0;
    for (const [id, probe] of this.#pending) {
      if (now < probe.deadline) continue;
      this.#pending.delete(id);
      if (!probe.confirmed) {
        if (probe.measured) unsent++;
        evicted = head = true;
        continue;
      }
      this.#graveyard.set(id, probe.sentAt);
      if (this.#graveyard.size > GRAVEYARD_MAX)
        this.#graveyard.delete(this.#graveyard.keys().next().value!);
      if (probe.measured) this.#record(probe, probe.deadline, true, null);
      evicted = true;
      head ||= id === this.#head;
    }
    if (unsent) {
      this.#flush();
      this.io.post({ type: "interrupted", count: unsent, reason: "send-failed" });
    }
    // A timeout completes a chain step as a reply would; a write given up wakes a chain its full window put to sleep.
    if (evicted) this.#step(now, head);
  }

  /** The RTO within 8 ms and 1 s, so a reply-driven chain covers a late reply well before its deadline. */
  #backupMs(): number {
    return this.#rtt.deadlineMs(BACKUP_FLOOR_MS, BACKUP_CEILING_MS);
  }

  #record(probe: Probe, observedAt: number, timedOut: boolean, handlingMs: number | null): void {
    this.#outbox.push({
      rttMs: observedAt - probe.sentAt,
      timedOut,
      handlingMs,
      observedAt: this.clock.origin + observedAt,
      inWindow: observedAt <= probe.cutoff,
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
    this.io.hangUp();
  }

  #drained(now: number): void {
    this.#sweep(now);
    this.#flush();
    this.#draining = false;
    this.io.post({ type: "drained" });
  }

  /** Ends a finished drain, or arms the timer for the earliest thing due; an early firing just re-arms. */
  #settle(now: number): void {
    if (this.#state === "stopped") return;
    let drainAt = Infinity;
    if (this.#draining) {
      drainAt = -Infinity;
      for (const probe of this.#pending.values())
        if (probe.cutoff < Infinity) drainAt = Math.max(drainAt, probe.deadline);
      if (now >= drainAt) {
        this.#drained(now);
        drainAt = Infinity;
      }
    }
    let at = Math.min(this.#flushAt, drainAt);
    for (const probe of this.#pending.values()) at = Math.min(at, probe.deadline);
    const { intervalMs, maxInFlight } = this.#pace;
    // A backup due while the window is full puts the chain to sleep; a full cadence waits for a free slot.
    if (this.#state === "open")
      at = Math.min(
        at,
        intervalMs === null
          ? this.#backupAt
          : this.#pending.size < maxInFlight
            ? this.#sentAt + intervalMs
            : Infinity,
      );
    if (at >= this.#timerAt) return;
    this.#timerAt = at;
    this.clock.arm(Math.max(0, at - now));
  }
}
