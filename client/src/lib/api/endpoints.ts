import type { LatencyEndpoint, ThroughputEndpoint } from "./preflight";

/* A target is a preflight endpoint made addressable: the wire fields minus
 * `baseUrl`, plus the absolute origin that base URL resolved against the page,
 * the transport that reaches it, and its route paths. Preflight advertises
 * origins only — the paths come from the client's own table in
 * runner/real/backendPure.ts — so the target has to carry them. `baseUrl` stays
 * as the unresolved wire value on targets built from a preflight response and is
 * absent on ones built by hand. */

export interface FetchThroughputTarget extends Omit<
  ThroughputEndpoint,
  "baseUrl"
> {
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
  baseUrl?: string;
  id: string;
  origin: string;
  transport: "websocket";
  protocol: "http1";
  tls: boolean;
  routes: { probe: string; ping: string };
}

export type LatencyTarget = WebSocketLatencyTarget;
