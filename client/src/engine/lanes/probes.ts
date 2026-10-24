// A latency bus to one server; its worker paces probes, and this side redials it and hands it between owners.
import type { ProbeCommand, ProbeReport } from "../../protocol/messages";
import { workerScript } from "../../lib/trusted";
import { ROUTES } from "../../protocol/routes";
import { DEADLINE_CEILING_MS } from "../measure/latency";
import { delay, fromEpoch } from "../run/clock";
import { handshakes, THROTTLED } from "../net/webtransport";
import { answered } from "./endings";
import {
  SignInRequired,
  type BusSlot,
  type LatencyBus,
  type LatencyTarget,
  type OpenProbeChannel,
  type ProbeChannelEvents,
  type ProbeChannelOptions,
} from "../types";

/** A draining worker gets its probes' longest deadline and this to acknowledge. */
const STOP_MARGIN_MS = 250;

export function openProbeChannel(
  { target, auth, control, intervalMs, maxInFlight }: ProbeChannelOptions,
  events: ProbeChannelEvents,
): LatencyBus {
  const websocket = target.transport === "websocket";
  const route = websocket ? ROUTES.ping : ROUTES.wtPing;
  const released = new AbortController();
  /** A window is open or draining, so cutting the bus off leaves its population incomplete. */
  let measured = false;
  let drain: { promise: Promise<void>; acknowledged: AbortController } | null = null;
  let remeasured = false;
  let stops = 0;
  /** A new owner is told at once that the bus answers on its connection, or why it stalled. */
  let answering = false;
  let stall: string | null = null;
  /** Each owner may redial a stalled bus once, after the server answered over HTTP. */
  let redials = 1;
  let waiting = false;
  /** A WebTransport dial is out, so ending the channel now abandons it, which Chrome counts as a failure. */
  let dialing = false;
  const worker = new Worker(workerScript(WORKERS.probe), { type: "module" });

  const redial = async () => {
    if (!websocket && handshakes.throttled.peek()) {
      stall = THROTTLED;
      return events.stalled(THROTTLED);
    }
    if (!redials || waiting) return;
    redials--;
    waiting = true;
    // Asking over HTTP dials nothing, so a restarting server is asked until it answers.
    await answered(control, auth, released.signal);
    waiting = false;
    if (stall !== null && !released.signal.aborted) {
      dialing = !websocket;
      worker.postMessage({ type: "dial" } satisfies ProbeCommand);
    }
  };

  const release = () => {
    if (dialing) handshakes.failed();
    dialing = false;
    released.abort();
    worker.onmessage = worker.onerror = null;
    worker.terminate();
  };
  // Outcomes went down with the worker, so the population can no longer be complete.
  const cutOff = () => {
    if (measured) events.incomplete?.();
    release();
  };
  const signedOut = () => {
    events.signInRequired();
    release();
  };

  worker.onmessage = ({ data }: MessageEvent<ProbeReport>) => {
    switch (data.type) {
      case "ready":
        dialing = false;
        answering = true;
        return events.ready();
      case "outcomes":
        for (const outcome of data.outcomes) outcome.observedAt = fromEpoch(outcome.observedAt);
        return events.outcomes(data.outcomes);
      case "interrupted":
        return events.interrupted(data.count, data.reason);
      case "stalled":
        answering = false;
        stall = data.detail;
        events.stalled(data.detail);
        void redial();
        // A refused WebSocket upgrade looks like any break, so each outage asks whether the sign-in ended.
        return void auth.ended(released.signal).then(
          (ended) => {
            if (ended && !released.signal.aborted) signedOut();
          },
          () => {},
        );
      case "resumed":
        answering = true;
        stall = null;
        return events.resumed();
      case "sign-in":
        return signedOut();
      case "handshake":
        dialing = false;
        return data.opened || handshakes.failed();
      case "drained":
        if (data.id !== stops) return;
        if (!remeasured) measured = false;
        return drain?.acknowledged.abort();
      case "renewing":
        answering = false;
        dialing = !websocket;
        return;
    }
  };
  worker.onerror = ({ message }) => {
    cutOff();
    events.stalled(`latency worker failed: ${message}`);
  };

  try {
    if (!websocket && handshakes.throttled.peek()) throw new Error(THROTTLED);
    const url = `${websocket ? target.origin.replace(/^http/, "ws") : target.origin}${route}`;
    dialing = !websocket;
    worker.postMessage({
      type: "start",
      url: auth.socket(url),
      transport: target.transport,
      intervalMs,
      maxInFlight,
    } satisfies ProbeCommand);
  } catch (error) {
    release();
    if (!(error instanceof SignInRequired)) throw error;
    queueMicrotask(() => events.signInRequired());
  }

  return {
    measure() {
      if (released.signal.aborted) return;
      measured = true;
      remeasured = drain !== null;
      worker.postMessage({ type: "measure" } satisfies ProbeCommand);
    },
    pace(intervalMs, maxInFlight) {
      if (!released.signal.aborted)
        worker.postMessage({ type: "pace", intervalMs, maxInFlight } satisfies ProbeCommand);
    },
    stop() {
      if (released.signal.aborted) return Promise.resolve();
      if (drain && !remeasured) return drain.promise;
      drain?.acknowledged.abort();
      remeasured = false;
      worker.postMessage({ type: "stop", id: ++stops } satisfies ProbeCommand);
      const acknowledged = new AbortController();
      const bound = AbortSignal.any([released.signal, acknowledged.signal]);
      const promise = delay(DEADLINE_CEILING_MS + STOP_MARGIN_MS, bound).then(() => {
        if (drain?.promise === promise) drain = null;
        if (!bound.aborted) cutOff();
      });
      drain = { promise, acknowledged };
      return promise;
    },
    dispose(incomplete = false) {
      if (released.signal.aborted) return;
      if (incomplete) cutOff();
      else release();
    },
    renew() {
      if (!released.signal.aborted) worker.postMessage({ type: "renew" } satisfies ProbeCommand);
    },
    adopt(next) {
      if (released.signal.aborted) return false;
      events = next;
      redials = 1;
      if (stall !== null) {
        next.stalled(stall);
        void redial();
      } else if (answering) next.ready();
      return true;
    },
  };
}

export const IDLE_INTERVAL_MS = 1_000;

export function createBusSlot(open: OpenProbeChannel, parked: ProbeChannelEvents): BusSlot {
  let held: { target: LatencyTarget; bus: LatencyBus } | null = null;
  const slot: BusSlot = {
    adopt(target, events) {
      if (held?.target.id !== target.id) return null;
      if (held.bus.adopt(events)) return held.bus;
      held = null;
      return null;
    },
    take(options, events) {
      const adopted = slot.adopt(options.target, events);
      if (adopted) {
        adopted.pace(options.intervalMs, options.maxInFlight);
        return adopted;
      }
      slot.dispose();
      const bus = open(options, events);
      held = { target: options.target, bus };
      return bus;
    },
    renew: () => held?.bus.renew(),
    park() {
      const bus = held?.bus;
      if (!bus) return;
      if (!bus.adopt(parked)) return void (held = null);
      if (held?.bus !== bus) return;
      void bus.stop();
      bus.pace(IDLE_INTERVAL_MS, 2);
    },
    dispose(bus = held?.bus) {
      if (!held || held.bus !== bus) return;
      held.bus.dispose();
      held = null;
    },
  };
  return slot;
}
