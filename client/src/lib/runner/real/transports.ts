// What each measurement transport is. Adding a kind is a row here plus its lane
// implementation, and Record<TransportKind, TransportSpec> makes a missing row a
// build error. Lane policy and presentation still switch on the kind explicitly
// — streamPolicy, backendPure, RealRunner, targetPresentation, and the endpoint
// and setup components — so a new kind also needs those branches read.
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

/** Whether a transfer kind rides a session rather than fetch requests. */
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
