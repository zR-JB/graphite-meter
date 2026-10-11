// A latency bus to one server, driven by a probe worker: the worker paces, times and resolves probes and reconnects
// itself; this side translates their times, asks whether a broken bus was the sign-in ending, and bounds the drain.
import type { ProbeCommand, ProbeReport } from "../../protocol/messages";
import { ROUTES } from "../../protocol/routes";
import { DEADLINE_CEILING_MS } from "../measure/latency";
import { fromEpoch } from "../run/clock";
import { SignInRequired, type OpenProbeChannel } from "../types";
import { delay } from "./wait";

/** A draining worker gets its probes' longest deadline and this to acknowledge. */
const STOP_MARGIN_MS = 250;

export const openProbeChannel: OpenProbeChannel = ({ target, auth, intervalMs, maxInFlight }, events) => {
  const websocket = target.transport === "websocket";
  const route = websocket ? ROUTES.ping : ROUTES.wtPing;
  const released = new AbortController();
  let state: "probing" | "stopping" | "released" = "probing";
  let measured = false;
  let stopping: Promise<void> | undefined;
  const worker = new Worker(WORKERS.probe, { type: "module" });

  const release = () => {
    state = "released";
    released.abort();
    worker.onmessage = worker.onerror = null;
    worker.terminate();
  };
  // Outcomes went down with the worker, so the population can no longer be complete.
  const lost = () => {
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
        return events.ready();
      case "outcomes":
        for (const outcome of data.outcomes) outcome.observedAt = fromEpoch(outcome.observedAt);
        return events.outcomes(data.outcomes);
      case "interrupted":
        return events.interrupted(data.count, data.reason);
      case "stalled":
        events.stalled(data.detail);
        // A refused WebSocket upgrade looks like any break, so each outage asks whether the sign-in ended.
        return void auth.ended(released.signal).then(
          (ended) => {
            if (ended && !released.signal.aborted) signedOut();
          },
          () => {},
        );
      case "resumed":
        return events.resumed();
      case "sign-in":
        return signedOut();
      case "stopped":
        return release();
    }
  };
  worker.onerror = ({ message }) => {
    lost();
    events.stalled(`latency worker failed: ${message}`);
  };

  try {
    const url = `${websocket ? target.origin.replace(/^http/, "ws") : target.origin}${route}`;
    const ticket = auth.ticket(target.origin, route);
    worker.postMessage({
      type: "start",
      url,
      transport: target.transport,
      ticket,
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
      if (state !== "probing") return;
      measured = true;
      worker.postMessage({ type: "measure" } satisfies ProbeCommand);
    },
    pace(intervalMs) {
      if (state === "probing") worker.postMessage({ type: "pace", intervalMs } satisfies ProbeCommand);
    },
    stop() {
      return (stopping ??= (async () => {
        if (state !== "probing") return;
        state = "stopping";
        worker.postMessage({ type: "stop" } satisfies ProbeCommand);
        await delay(DEADLINE_CEILING_MS + STOP_MARGIN_MS, released.signal);
        if (!released.signal.aborted) lost();
      })());
    },
    dispose(incomplete = false) {
      if (state === "released") return;
      if (incomplete) lost();
      else release();
    },
  };
};
