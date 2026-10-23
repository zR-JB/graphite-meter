// Fakes for driving a real run: page time that moves only when a test says so, and servers reporting at a fixed rate.
import type {
  Checkpoint,
  Discovery,
  FetchTarget,
  PreparedServer,
  ServerAuth,
  StageEvents,
  StageResources,
  StageSetup,
} from "../types";
import type { Clock } from "./clock";
import type { Activity } from "./schedule";

interface Timer {
  at: number;
  id: number;
  fn: () => void;
}

export class FakeClock implements Clock {
  t = 0;
  #timers: Timer[] = [];
  #ids = 0;
  now = () => this.t;
  epoch = () => 1_760_000_000_000 + this.t;
  after = (ms: number, fn: () => void) => {
    const timer = { at: this.t + Math.max(0, ms), id: ++this.#ids, fn };
    this.#timers.push(timer);
    return () => {
      const index = this.#timers.indexOf(timer);
      if (index >= 0) this.#timers.splice(index, 1);
    };
  };

  every(ms: number, fn: () => void): () => void {
    let cancel = () => {};
    const tick = () => {
      cancel = this.after(ms, tick);
      fn();
    };
    cancel = this.after(ms, tick);
    return () => cancel();
  }

  async advance(ms: number): Promise<void> {
    const end = this.t + ms;
    for (;;) {
      await settled();
      let next: Timer | undefined;
      for (const timer of this.#timers) if (!next || timer.at < next.at) next = timer;
      if (!next || next.at > end) break;
      this.#timers.splice(this.#timers.indexOf(next), 1);
      this.t = Math.max(this.t, next.at);
      next.fn();
    }
    this.t = end;
    await settled();
  }
}

const settled = () => new Promise((resolve) => setImmediate(resolve));

export interface Peer {
  id: string;
  /** Bytes per millisecond in each direction. */
  rate?: number;
  latency?: boolean;
  silent?(activity: Activity, measuredMs: number, dir: "down" | "up"): boolean;
  prepare?(activity: Activity, events: StageEvents): void | Promise<void>;
  replanned?(): void;
  measure?(events: StageEvents, activity: Activity): void;
  finish?(events: StageEvents): void | Promise<void>;
  discard?(events: StageEvents, incomplete: boolean): void;
  checkpoint?(signal: AbortSignal): Promise<Checkpoint | null> | undefined;
  /** A measuring checkpoint answers the count at the moment it was asked, this long after. */
  checkpointLagMs?: number;
  replaceUpload?(signal: AbortSignal): Promise<void>;
  countAt?(at: number): number | null;
  /** The lanes report what they counted but had not reported yet, and may answer the time their count stands at. */
  flush?(events: StageEvents, cutoff: number): void | number | Promise<void | number>;
  receives?: false;
  /** The server's auth check answers this when asked whether its sign-in or grant has ended. */
  signedOut?: boolean;
}

export function prepared(id: string, latency: boolean, signedOut = false): PreparedServer {
  const origin = `https://${id}.example`;
  const fetch: FetchTarget = {
    id: origin,
    origin,
    transport: "fetch-stream",
    protocol: "http1",
    tls: true,
  };
  const probe = {
    clientIp: "192.0.2.1",
    clientIpVersion: 4,
    clientIpSource: "socket",
    protocolNegotiated: "http/1.1",
  } as const;
  const verified = {
    generation: "g",
    verifiedAt: 0,
    verifiedFor: { selection: "auto", head: origin },
  };
  return {
    server: { id, url: origin, name: id },
    auth: { ended: async () => signedOut } as Partial<ServerAuth> as ServerAuth,
    discovery: { engineVersion: "test" } as Discovery,
    throughput: { target: fetch, fetch, probe, browserProtocol: "http/1.1", ...verified },
    latency: latency
      ? {
          target: { id: origin, origin, transport: "websocket", protocol: "http1", tls: true },
          probe,
          rttMs: 10,
          ...verified,
        }
      : null,
  };
}

/** Lanes report every 20 ms at `rate` bytes per ms, receivers every 100 ms, from the window on. */
export function resources(
  peer: Peer,
  setup: StageSetup,
  calls: string[],
  clock: FakeClock,
): StageResources {
  const { activity, events } = setup;
  const rate = peer.rate ?? 1;
  const tag = `${peer.id}:${activity.stage}`;
  let measuring = false;
  let started = 0;
  let stopLanes = () => {};
  const receiver = (): Checkpoint => {
    const ms = clock.t - started;
    return {
      id: tag,
      bytes: Math.round(ms * rate),
      nanos: Math.round(ms * 1e6) + 1,
      receivedAtMs: setup.now(),
    };
  };
  const stop = () => {
    measuring = false;
    stopLanes();
  };
  return {
    async prepare() {
      calls.push(`begin:${tag}`);
      await peer.prepare?.(activity, events);
    },
    replanned() {
      peer.replanned?.();
    },
    measure() {
      measuring = true;
      started = clock.t;
      calls.push(`measure:${peer.id}`);
      if (activity.transfer.length)
        stopLanes = clock.every(20, () => {
          const ms = clock.t - started;
          const moves = (dir: "down" | "up") =>
            activity.transfer.includes(dir) && !peer.silent?.(activity, ms, dir);
          if (moves("down")) events.download(rate * 20);
          if (moves("up") && peer.receives !== false && ms % 100 < 20) events.receiver(receiver());
        });
      peer.measure?.(events, activity);
    },
    async finish() {
      stop();
      calls.push(`end:${peer.id}`);
      await peer.finish?.(events);
    },
    discard(incomplete = false) {
      stop();
      calls.push(`discard:${peer.id}`);
      peer.discard?.(events, incomplete);
    },
    // A receiver that is absent, or silent at this moment, answers no checkpoint either.
    checkpoint(signal) {
      const answer =
        peer.receives === false || peer.silent?.(activity, clock.t - started, "up")
          ? null
          : receiver();
      return (
        peer.checkpoint?.(signal) ??
        (measuring && peer.checkpointLagMs
          ? new Promise<Checkpoint | null>((resolve) =>
              clock.after(peer.checkpointLagMs!, () => resolve(answer)),
            )
          : Promise.resolve(answer))
      );
    },
    countAt: async (at) => peer.countAt?.(at) ?? null,
    flush: async (cutoff) => (await peer.flush?.(events, cutoff)) ?? null,
    replaceUpload: (signal) => peer.replaceUpload?.(signal) ?? Promise.resolve(),
  };
}
