/* Targets are preflight endpoints made addressable: wire fields plus the
 * absolute origin, the transport that reaches it, and route paths. Preflight
 * advertises origins and transports only, so routes come from the client's own
 * table in runner/real/backendPure.ts. */

import type { LatencyEndpoint, ThroughputEndpoint } from "./preflight";

export interface FetchThroughputTarget extends Omit<
  ThroughputEndpoint,
  "baseUrl" | "transport"
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

/** WebTransport throughput rides HTTP/3 sessions. Upload ids are still minted
 *  and finalized over HTTP, so those routes come along. */
export interface WebTransportThroughputTarget extends Omit<
  ThroughputEndpoint,
  "baseUrl" | "transport" | "protocol"
> {
  baseUrl?: string;
  id: string;
  origin: string;
  transport: "webtransport";
  protocol: "http3";
  tls: boolean;
  routes: {
    probe: string;
    wtSession: string;
    wtDownload: string;
    wtUpload: string;
    uploadSession: string;
    uploadProgress: string;
  };
}

export interface WebSocketLatencyTarget extends Omit<
  LatencyEndpoint,
  "baseUrl" | "transport"
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

export interface WebTransportLatencyTarget extends Omit<
  LatencyEndpoint,
  "baseUrl" | "transport"
> {
  baseUrl?: string;
  id: string;
  origin: string;
  transport: "webtransport";
  protocol: "http3";
  tls: boolean;
  routes: { probe: string; wtSession: string; wtPing: string };
}

export type LatencyTarget = WebSocketLatencyTarget | WebTransportLatencyTarget;
