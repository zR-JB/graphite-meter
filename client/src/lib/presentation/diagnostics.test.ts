import { expect, test } from "bun:test";
import { diagnosticReport, hostKind } from "./diagnostics";

const servers = [
  { id: "self", url: ".", name: "Attic NAS", location: "Berlin" },
  { id: "fra", url: "https://meter.example.net:8443", name: "Frankfurt" },
];

test("the report keeps every field but names no server, address or secret", () => {
  const report = diagnosticReport({
    page: "http://192.168.1.20:7246/#/endpoint",
    servers,
    clientIps: ["2001:db8::7"],
    state: {
      servers: {
        fra: {
          discovery: { server: { name: "Frankfurt" }, futureField: 42 },
          paths: {
            credentials: { kind: "grant", token: "secret-grant" },
            throughput: {
              target: { origin: "https://meter.example.net:8443" },
              probe: { clientIp: "2001:db8::7", clientIpVersion: 6 },
            },
          },
          message: "Sign in to Frankfurt at https://meter.example.net:8443/",
        },
      },
      selection: ["self", "fra", "france"],
      error: "2001:db8::7 refused by 192.168.1.20",
    },
  });
  for (const secret of [
    "secret-grant",
    "2001:db8::7",
    "meter.example.net",
    "192.168.1.20",
    "Frankfurt",
    "Attic NAS",
    "Berlin",
  ])
    expect(report).not.toContain(secret);
  const parsed = JSON.parse(report);
  expect(parsed.hosts).toEqual({
    "server-1": "private IPv4",
    "server-2": "name",
  });
  const fra = parsed.servers["server-2"];
  expect(fra.discovery).toEqual({
    server: { name: "Server 2" },
    futureField: 42,
  });
  expect(fra.paths.credentials).toBeUndefined();
  expect(fra.paths.throughput.target.origin).toBe("https://server-2:8443");
  expect(fra.paths.throughput.probe).toEqual({ clientIpVersion: 6 });
  expect(fra.message).toBe("Sign in to Server 2 at https://server-2:8443/");
  // A catalogue id is replaced as a word, never inside another one.
  expect(parsed.selection).toEqual(["self", "server-2", "france"]);
  expect(parsed.error).toBe("client-address refused by server-1");
});

test("hosts are described by their kind", () => {
  expect(
    [
      "localhost",
      "127.0.0.1",
      "[::1]",
      "10.1.2.3",
      "172.20.0.1",
      "100.64.0.9",
      "8.8.8.8",
      "[fd00::1]",
      "[2a00::1]",
      "nas.local",
      "meter.example.net",
    ].map(hostKind),
  ).toEqual([
    "loopback",
    "loopback IPv4",
    "loopback IPv6",
    "private IPv4",
    "private IPv4",
    "private IPv4",
    "public IPv4",
    "private IPv6",
    "public IPv6",
    "local name",
    "name",
  ]);
});
