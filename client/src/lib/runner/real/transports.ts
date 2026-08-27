// What each measurement transport is.
import type { TransportKind } from "../contract";

interface TransportSpec {
  kind: TransportKind;
  /** Which role a kind can serve. */
  role: "throughput" | "latency" | "both";
/* Whether this browser has the API at all. */
  usable(): boolean;
  /** Whether the bytes ride a session rather than one request per lane. */
  ridesSession: boolean;
  /** Picker order within an origin. */
  order: number;
}

const hasWebTransport = (): boolean => typeof WebTransport !== "undefined";
const always = (): boolean => true;

/* Why this browser cannot drive WebTransport: a page served over plain http, or a browser that never shipped the. */
type WebTransportGap = "insecure-page" | "no-api";

/* Which reason applies, or null when WebTransport is reachable. */
export function webTransportGap(): WebTransportGap | null {
  if (hasWebTransport()) return null;
  return globalThis.isSecureContext === false ? "insecure-page" : "no-api";
}

/* The server clamps both directions here (wire.WTMaxStreams), refusing an upload lane past it. */
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

/* A kind added with ridesSession false and no fetch lane of its own therefore compiles clean and carries its. */
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
