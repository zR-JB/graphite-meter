import type { LatencyEndpoint, ThroughputEndpoint } from "./preflight";

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

export type ThroughputTarget = FetchThroughputTarget;
export type LatencyTarget = WebSocketLatencyTarget;
