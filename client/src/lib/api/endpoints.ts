/* Advertised endpoints made addressable: an absolute origin and the persisted selection id. */
import type { ProtocolTarget } from "../runner/contract";

interface Target<Transport extends string, Protocol extends ProtocolTarget> {
  id: string;
  origin: string;
  transport: Transport;
  protocol: Protocol;
  tls: boolean;
}

export type FetchThroughputTarget = Target<"fetch-stream", ProtocolTarget>;
/* WebTransport throughput rides HTTP/3 sessions, as raw streams or as the experimental datagram flood. */
export type WebTransportThroughputTarget = Target<
  "webtransport" | "webtransport-datagram",
  "http3"
>;
export type LatencyTarget =
  Target<"websocket", "http1"> | Target<"webtransport", "http3">;
