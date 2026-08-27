import { expect, test } from "bun:test";
import {
  compensationTransportFromProtocol,
  compensationTransportLabel,
  httpProtocolLabel,
  normalizeHttpProtocol,
} from "./protocol";
test("protocol identifiers share one canonical mapping", () => {
  for (const value of ["http1", "http/1.1", "HTTP/1.1"])
    expect(normalizeHttpProtocol(value)).toBe("http1");
  for (const value of ["http2", "h2", "h2c", "HTTP/2.0"])
    expect(normalizeHttpProtocol(value)).toBe("http2");
  for (const value of ["http3", "h3", "h3-29", "HTTP/3.0"])
    expect(normalizeHttpProtocol(value)).toBe("http3");
  expect(normalizeHttpProtocol("unknown")).toBeUndefined();
});
test("canonical protocols drive labels and compensation transport", () => {
  expect(httpProtocolLabel("h2")).toBe("HTTP/2");
  expect(httpProtocolLabel("negotiated")).toBe("Negotiated HTTP");
  expect(compensationTransportFromProtocol("h3-29", true)).toBe("http3-quic");
  expect(compensationTransportFromProtocol(undefined, false)).toBe(
    "http1-clear",
  );
  expect(compensationTransportLabel("https-tls")).toBe("HTTP/1.1 TLS");
});
