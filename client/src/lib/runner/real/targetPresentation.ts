import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../../api/endpoints";
import type { ProtocolTarget, TransportDiscovery } from "../contract";
import { httpProtocolLabel } from "../protocol";

export interface TargetPresentation {
  label: string;
  summary: string;
  advertisedDetail: string;
}

export function describeTarget(
  discovery: TransportDiscovery,
  target: FetchThroughputTarget | WebSocketLatencyTarget,
  observedProtocol?: ProtocolTarget,
): TargetPresentation {
  const security = target.tls ? "TLS" : "clear";
  if (target.transport === "websocket") {
    const nativeH1 =
      discovery.throughput[target.origin]?.target?.protocol === "http1";
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
