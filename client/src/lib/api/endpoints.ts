/* Targets are preflight endpoints made addressable: wire fields plus the
 * absolute origin, the transport that reaches it, and route paths. Preflight
 * advertises origins only, so routes come from the client's own table in
 * runner/real/backendPure.ts. */

import type { LatencyEndpoint, ThroughputEndpoint } from "./preflight";

export interface FetchThroughputTarget extends Omit<
  ThroughputEndpoint,
  "baseUrl"
> {
  /** Unresolved wire value, absent on hand built targets. */
  baseUrl?: string;
  id: string;
  origin: string;
  transport: "fetch-stream";
  tls: boolean;
  routes: {
    probe: string;
    download: string;
    upload: string;
    uploadSession: string;
    uploadProgress: string;
  };
}

export interface WebSocketLatencyTarget extends Omit<
  LatencyEndpoint,
  "baseUrl"
> {
  /** Unresolved wire value, absent on hand built targets. */
  baseUrl?: string;
  id: string;
  origin: string;
  transport: "websocket";
  protocol: "http1";
  tls: boolean;
  routes: { probe: string; ping: string };
}

export type LatencyTarget = WebSocketLatencyTarget;
