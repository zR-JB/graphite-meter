import { expect, test } from "bun:test";
import { diagnosticReport, hostKind } from "./report";

test("the report keeps every field but names no server, address or secret", () => {
  const report = diagnosticReport({
    page: "http://192.168.1.20:7246/#/endpoint",
    servers: [
      { id: "self", url: "http://192.168.1.20:7246", name: "Attic NAS", location: "Berlin" },
      { id: "fra", url: "https://meter.example.net:8443", name: "Frankfurt" },
      { id: "fra", url: "https://meter.example.net:8443", name: "Frankfurt Edge" },
    ],
    clientIps: ["2001:db8::7"],
    state: {
      servers: new Map([
        [
          "fra",
          {
            discovery: { server: { name: "Frankfurt Edge" }, futureField: 42 },
            throughput: new Map([["https://meter.example.net:8443", { blocked: null }]]),
            paths: {
              credentials: { kind: "grant", token: "secret-grant" },
              probe: { clientIp: "2001:db8::7", clientIpVersion: 6 },
            },
            message: "Sign in to Frankfurt at https://meter.example.net:8443/",
          },
        ],
      ]),
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
    "Attic",
    "Berlin",
  ])
    expect(report).not.toContain(secret);
  const parsed = JSON.parse(report);
  expect(parsed.hosts).toEqual({ "server-1": "private IPv4", "server-2": "name" });
  const fra = parsed.servers["server-2"];
  expect(fra.discovery).toEqual({ server: { name: "Server 2" }, futureField: 42 });
  expect(fra.throughput).toEqual({ "https://server-2:8443": { blocked: null } });
  expect(fra.paths).toEqual({ probe: { clientIpVersion: 6 } });
  expect(fra.message).toBe("Sign in to Server 2 at https://server-2:8443/");
  // A catalog ID is replaced as a word, never inside another one.
  expect(parsed.selection).toEqual(["self", "server-2", "france"]);
  expect(parsed.error).toBe("client-address refused by server-1");
});

test("hosts are described by their kind", () => {
  const hosts = [
    ["localhost", "loopback"],
    ["127.0.0.1", "loopback IPv4"],
    ["[::1]", "loopback IPv6"],
    ["10.1.2.3", "private IPv4"],
    ["172.20.0.1", "private IPv4"],
    ["100.64.0.9", "private IPv4"],
    ["8.8.8.8", "public IPv4"],
    ["[fd00::1]", "private IPv6"],
    ["[2a00::1]", "public IPv6"],
    ["nas.local", "local name"],
    ["meter.example.net", "name"],
  ];
  for (const [host, kind] of hosts) expect(hostKind(host!), host).toBe(kind!);
});
