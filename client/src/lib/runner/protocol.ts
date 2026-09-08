import type { CompensationTransport, ProtocolTarget } from "./contract";

/* Canonicalize current Resource Timing and API/configured protocol tokens. */
export function normalizeHttpProtocol(
  protocol?: string,
): ProtocolTarget | undefined {
  if (protocol === "http1" || protocol === "http/1.1") return "http1";
  if (protocol === "http2" || protocol === "h2") return "http2";
  if (protocol === "http3" || protocol === "h3") return "http3";
  return protocol === "negotiated" ? "negotiated" : undefined;
}

export function httpProtocolLabel(protocol?: string): string {
  const normalized = normalizeHttpProtocol(protocol);
  if (normalized === "http1") return "HTTP/1.1";
  if (normalized === "http2") return "HTTP/2";
  if (normalized === "http3") return "HTTP/3";
  if (normalized === "negotiated") return "Negotiated HTTP";
  return protocol ?? "unknown";
}

export function compensationTransportFromProtocol(
  protocol: string | undefined,
  secure: boolean,
): CompensationTransport {
  const normalized = normalizeHttpProtocol(protocol);
  if (normalized === "http3") return "http3-quic";
  if (normalized === "http2") return "http2";
  return secure ? "https-tls" : "http1-clear";
}

export function compensationTransportLabel(
  transport: CompensationTransport,
): string {
  if (transport === "http1-clear") return `${httpProtocolLabel("http1")} clear`;
  if (transport === "https-tls") return `${httpProtocolLabel("http1")} TLS`;
  if (transport === "http2") return httpProtocolLabel("http2");
  return `${httpProtocolLabel("http3")} QUIC`;
}
