// What each measurement transport is. Record<TransportKind, TransportSpec>
// makes a missing row here a build error; it does not make the branches one.
// A new kind needs every site that switches on the kind read:
//   contract.ts, api/preflight.ts, api/endpoints.ts, real/backendPure.ts,
//   real/streamPolicy.ts, real/targetPresentation.ts, real/transportViewModel.ts,
//   RealRunner.ts, connectionModel.ts, workers/ping-worker.ts,
//   components/EndpointInfo.svelte, components/settings/TestSetupPanel.svelte
// real/latencyChannel.ts is the one that fails quietly: an if/else on
// "webtransport", so an unhandled kind takes the WebSocket branch.
import type { TransportKind } from "../contract";

export interface TransportSpec {
  kind: TransportKind;
  /** Which role a kind can serve. */
  role: "throughput" | "latency" | "both";
  /** Whether this browser has the API at all. Called per use, not frozen at
   *  module load: an API's presence is an environment fact. */
  usable(): boolean;
  /** Whether the bytes ride a session rather than one request per lane. */
  ridesSession: boolean;
  /** Picker order within an origin. */
  order: number;
}

const hasWebTransport = (): boolean => typeof WebTransport !== "undefined";
const always = (): boolean => true;

/** Why this browser cannot drive WebTransport: a page served over plain http,
 *  or a browser that never shipped the API. */
export type WebTransportGap = "insecure-page" | "no-api";

/** Which reason applies, or null when WebTransport is reachable.
 *
 *  `WebTransport` is [SecureContext], so an http page has no such global —
 *  indistinguishable by presence alone from Safari, and only one of the two is
 *  the reader's to fix. A context declaring no secure-context flag at all (a
 *  test runner) reads as the browser gap, the honest answer where there is no
 *  page. */
export function webTransportGap(): WebTransportGap | null {
  if (hasWebTransport()) return null;
  return globalThis.isSecureContext === false ? "insecure-page" : "no-api";
}

/** The server clamps both directions here (wire.WTMaxStreams), refusing an
 *  upload lane past it. Callers clamp against it directly; see streamPolicy. */
export const WT_MAX_LANES = 16;

export const TRANSPORTS: Record<TransportKind, TransportSpec> = {
  "fetch-stream": {
    kind: "fetch-stream",
    role: "throughput",
    usable: always,
    ridesSession: false,
    order: 0,
  },
  websocket: {
    kind: "websocket",
    role: "latency",
    usable: always,
    ridesSession: false,
    order: 0,
  },
  webtransport: {
    kind: "webtransport",
    role: "both",
    usable: hasWebTransport,
    ridesSession: true,
    order: 1,
  },
  "webtransport-datagram": {
    kind: "webtransport-datagram",
    role: "throughput",
    usable: hasWebTransport,
    ridesSession: true,
    order: 2,
  },
};

/** Whether this client can drive a kind the server advertised. */
export function transportRunnable(kind: TransportKind): boolean {
  return TRANSPORTS[kind].usable();
}

/** Whether a transfer kind rides a session rather than fetch requests.
 *
 *  The flag decides only which of two carriers RealRunner reaches for
 *  (#primeTransfer takes #wtThroughputTarget when it is set), and there is no
 *  third: #committedKind returns "fetch-stream" for everything that is not a
 *  resolved session target. A kind added with ridesSession false and no fetch
 *  lane of its own therefore compiles clean and carries its bytes over fetch
 *  lanes, measuring a transport nobody selected. */
export function ridesSession(kind: TransportKind): boolean {
  return TRANSPORTS[kind].ridesSession;
}

/** Kinds a role can carry, in picker order. */
export function kindsForRole(role: "throughput" | "latency"): TransportKind[] {
  return Object.values(TRANSPORTS)
    .filter((spec) => spec.role === role || spec.role === "both")
    .sort((a, b) => a.order - b.order)
    .map((spec) => spec.kind);
}
