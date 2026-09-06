import { expect, test } from "bun:test";
import { historyServers } from "./servers";
import type { MultiServerResult } from "../servers/measurement";

const selection = [
  { id: "self", name: "Home", location: "Berlin", url: "https://home.test" },
  {
    id: "peer",
    name: "Frankfurt",
    location: "Frankfurt",
    url: "https://peer.test:8443",
  },
];
function details(primary = false): MultiServerResult {
  return {
    selection,
    participants: ["peer"],
    latencyFocus: "self",
    intervals: [],
    omittedIntervals: 0,
    failures: [],
    servers: selection.map((server, i) => ({
      server,
      throughput: {
        origin: server.url,
        transport: "fetch-stream",
        protocol: "http2",
      },
      latencyTarget:
        primary && i === 0
          ? null
          : { origin: server.url, transport: "websocket" },
      latency: null,
      latencyByStage: {
        latency: null,
        download: null,
        upload: null,
        bidirectional: null,
      },
      bufferbloat: null,
      download: null,
      upload: null,
      bidirectional: null,
      totalBytes: { down: 0, up: 0 },
    })),
  };
}
const legacy = { name: "Old server", location: "Berlin", engine: "old" };

test("legacy history retains its saved identity without inventing an address", () => {
  expect(historyServers({ server: legacy })).toEqual([
    { id: "single", label: "Old server · Berlin", host: null, ping: false },
  ]);
});
test("history lists the saved selection, including participants lost during a partial run", () => {
  expect(historyServers({ server: legacy, multiServer: details() })).toEqual([
    { id: "self", label: "Home · Berlin", host: "home.test", ping: false },
    { id: "peer", label: "Frankfurt", host: "peer.test:8443", ping: false },
  ]);
});
test("only a unique measurement ping target is marked, independently of the viewed server", () => {
  expect(
    historyServers({ server: legacy, multiServer: details(true) }).map(
      (server) => server.ping,
    ),
  ).toEqual([false, true]);
});
test("a single server avoids redundant address and ping text", () => {
  const multiServer = details();
  multiServer.selection = [
    { id: "self", name: "home.test", url: "https://home.test" },
  ];
  multiServer.servers = multiServer.servers.slice(0, 1);
  expect(historyServers({ server: legacy, multiServer })).toEqual([
    { id: "self", label: "home.test", host: null, ping: false },
  ]);
});
