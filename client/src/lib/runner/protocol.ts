import type { CompensationTransport, ProtocolTarget } from "./contract";

/* Canonicalize protocol identifiers from Resource Timing, Go net/http, and configured targets into one. */
export function normalizeHttpProtocol(
  protocol?: string,
): ProtocolTarget | undefined {
  const value = protocol?.toLowerCase();
  if (value === "http1" || value === "http/1.1") return "http1";
  if (
    value === "http2" ||
    value === "h2" ||
    value === "h2c" ||
    value === "http/2.0"
  )
    return "http2";
  if (
    value === "http3" ||
    value === "h3" ||
    value?.startsWith("h3-") ||
    value === "http/3.0"
  )
    return "http3";
  return value === "negotiated" ? "negotiated" : undefined;
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
