// What each measurement transport is. Adding a kind is a row here plus its lane
// implementation, and Record<TransportKind, TransportSpec> makes a missing row a
// build error.
//
// It does NOT make the branches a build error. Everything below switches on the
// kind explicitly, and each site was verified with git grep; a new kind needs
// every one of them read:
//   contract.ts             the TransportKind union itself
//   api/preflight.ts        wire endpoint variants, per role
//   api/endpoints.ts        the target interface each kind resolves to
//   real/backendPure.ts     classification, selection ids, per-role selection
//   real/streamPolicy.ts    lane counts and the policy label
//   real/latencyChannel.ts  bus URL and token mint — an if/else on
//                           "webtransport", so an unhandled kind silently takes
//                           the WebSocket branch rather than failing
//   real/targetPresentation.ts  label / summary / settings detail
//   real/transportViewModel.ts  which cards a role's picker lists
//   RealRunner.ts           role binding, the session/fetch dispatch, describe()
//   connectionModel.ts      resolving the committed target for the panel
//   workers/ping-worker.ts  which bus the worker dials
//   components/EndpointInfo.svelte, components/settings/TestSetupPanel.svelte
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
