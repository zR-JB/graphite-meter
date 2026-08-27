import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../../api/endpoints";
import type { ProtocolTarget, TransportDiscovery } from "../contract";
import { httpProtocolLabel } from "../protocol";

interface TargetPresentation {
  /** Compact chip text. */
  label: string;
  /** Connection-row text naming the mechanism as well as the protocol. */
  summary: string;
  /** Settings-option text, naming the origin the entry resolves to. */
  advertisedDetail: string;
}

/* A WebSocket claims HTTP/1.1 only when the throughput target sharing its origin names that protocol. */
export function describeTarget(
  discovery: TransportDiscovery,
  target: FetchThroughputTarget | WebTransportThroughputTarget | LatencyTarget,
  observedProtocol?: ProtocolTarget,
): TargetPresentation {
  const security = target.tls ? "TLS" : "clear";
  if (target.transport === "webtransport-datagram") {
    return {
      label: `WebTransport datagrams · ${security}`,
      summary: `WebTransport datagrams · ${httpProtocolLabel("http3")} · ${security}`,
      advertisedDetail: `Experimental unreliable-datagram flood over ${httpProtocolLabel("http3")} · ${target.origin}`,
    };
  }
  if (target.transport === "webtransport") {
    const mechanism = `WebTransport · ${httpProtocolLabel("http3")}`;
    // The throughput role rides QUIC streams; the latency role rides datagrams.
    if ("wtDownload" in target.routes) {
      return {
        label: `${mechanism} · ${security}`,
        summary: `WebTransport streams · ${httpProtocolLabel("http3")} · ${security}`,
        advertisedDetail: `QUIC stream session over ${httpProtocolLabel("http3")} · ${target.origin}`,
      };
    }
    return {
      label: `${mechanism} · ${security}`,
      summary: `${mechanism} datagrams · ${security}`,
      advertisedDetail: `Datagram bus over ${httpProtocolLabel("http3")} · ${target.origin}`,
    };
  }
  if (target.transport === "websocket") {
    const nativeH1 =
      discovery.throughput[target.origin]?.targets.find(
        (sibling) => sibling.transport === "fetch-stream",
      )?.protocol === "http1";
    const mechanism = nativeH1
      ? `WebSocket · ${httpProtocolLabel("http1")}`
      : "WebSocket";
    return {
      label: `${mechanism} · ${security}`,
      summary: `${mechanism} · ${security}`,
      advertisedDetail: `${nativeH1 ? `Direct ${httpProtocolLabel("http1")} WebSocket` : "WebSocket"} endpoint · ${target.origin}`,
    };
  }

  const protocol =
    target.protocol === "negotiated" && observedProtocol
      ? observedProtocol
      : target.protocol;
  return {
    label: `${httpProtocolLabel(protocol)} · ${security}`,
    summary: `Fetch stream · ${httpProtocolLabel(protocol)} · ${security}`,
    advertisedDetail:
      target.protocol === "negotiated"
        ? `Browser negotiates the available HTTP version · ${target.origin}`
        : `Direct ${httpProtocolLabel(target.protocol)} endpoint · ${target.origin}`,
  };
}
