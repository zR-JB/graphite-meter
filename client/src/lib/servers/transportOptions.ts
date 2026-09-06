import type { TransportDiscovery } from "../runner/contract";
import {
  selectLatencyTarget,
  selectThroughputTarget,
} from "../runner/real/backendPure";
import type { ServerIdentity } from "./catalog";

export interface ServerTransportOption {
  value: string;
  label: string;
  detail: string;
  disabled: boolean;
}

/** The same resolver supplies both compatibility evidence and the eventual prepared path. */
export function serverTransportOptions(
  role: "throughput" | "latency",
  servers: readonly ServerIdentity[],
  discoveries: Record<string, TransportDiscovery>,
  datagrams: boolean,
  selected: string,
  webTransport = typeof WebTransport !== "undefined",
): ServerTransportOption[] {
  const candidates =
    role === "throughput"
      ? [
          ["auto", "Automatic"],
          ["protocol:http1", "HTTP/1.1"],
          ["protocol:http2", "HTTP/2"],
          ["protocol:http3", "HTTP/3"],
          ["transport:webtransport", "WebTransport streams"],
          ...(datagrams || selected === "transport:webtransport-datagram"
            ? [["transport:webtransport-datagram", "WebTransport datagrams"]]
            : []),
        ]
      : [
          ["auto", "Automatic"],
          ["transport:websocket", "WebSocket"],
          ["transport:webtransport", "WebTransport"],
        ];
  if (!candidates.some(([value]) => value === selected))
    candidates.push([selected, "Selected origin"]);
  return candidates.map(([value, label]) => {
    const missing = servers.filter((server) => !discoveries[server.id]);
    const incompatible = servers.filter((server) => {
      const discovery = discoveries[server.id];
      return (
        discovery &&
        !(role === "throughput"
          ? selectThroughputTarget(discovery, value, webTransport)
          : selectLatencyTarget(discovery, value, webTransport))
      );
    });
    const detail =
      value === "auto"
        ? "Resolves independently for every selected server"
        : incompatible.length
          ? `Unavailable on ${incompatible.map((server) => server.name).join(", ")}`
          : missing.length
            ? `Checking ${missing.map((server) => server.name).join(", ")}`
            : `Available on all ${servers.length} selected servers`;
    return {
      value,
      label,
      detail,
      disabled:
        value !== "auto" && (missing.length > 0 || incompatible.length > 0),
    };
  });
}
