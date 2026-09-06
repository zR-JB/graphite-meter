import { expect, test } from "bun:test";
import { classifyTransportDiscovery } from "../runner/real/backendPure";
import { DEFAULT_CONFIG } from "../state/defaults";
import { testPreparedPaths } from "../runner/test-helpers.test";
import { serverTransportOptions } from "./transportOptions";
import { planServerStreams } from "./streamBudget";

const servers = [
  { id: "a", name: "A", url: "https://a.example" },
  { id: "b", name: "B", url: "https://b.example" },
];
const discoveries = Object.fromEntries(
  servers.map((server, index) => [
    server.id,
    classifyTransportDiscovery(
      [
        {
          baseUrl: server.url,
          transport: "fetch-stream",
          protocol: index ? "http2" : "http1",
        },
      ],
      [{ baseUrl: server.url, transport: "websocket" }],
      server.url,
      true,
    ),
  ]),
);
test("automatic may use different reliable protocols while explicit compatibility covers every server", () => {
  const options = serverTransportOptions(
    "throughput",
    servers,
    discoveries,
    false,
    "auto",
    false,
  );
  expect(options.find((option) => option.value === "auto")?.disabled).toBe(
    false,
  );
  expect(
    options.find((option) => option.value === "protocol:http1")?.detail,
  ).toBe("Unavailable on B");
  expect(
    options.find((option) => option.value === "protocol:http2")?.detail,
  ).toBe("Unavailable on A");
  expect(
    serverTransportOptions(
      "latency",
      servers,
      discoveries,
      false,
      "auto",
      false,
    ).find((option) => option.value === "transport:websocket")?.disabled,
  ).toBe(false);
  expect(
    options.some(
      (option) => option.value === "transport:webtransport-datagram",
    ),
  ).toBe(false);
});
test("shared H1 origins preserve progress and checkpoint capacity", () => {
  const paths = servers.map((server) => ({
    id: server.id,
    paths: testPreparedPaths(),
  }));
  const config = {
    ...structuredClone(DEFAULT_CONFIG),
    transferStreams: { mode: "auto" as const, count: 6 },
  };
  const activity = {
    stage: "upload" as const,
    transfer: ["up" as const],
    loadedLatency: false,
  };
  expect(planServerStreams(config, paths, activity)).toEqual({
    a: { down: 0, up: 2 },
    b: { down: 0, up: 1 },
  });
  const forced = {
    ...config,
    transferStreams: { mode: "forced" as const, count: 3 },
  };
  expect(() => planServerStreams(forced, paths, activity)).toThrow(
    "control capacity",
  );
});
test("four participants share a run-wide 128 stream ceiling", () => {
  const paths = Array.from({ length: 4 }, (_, i) => {
    const paths = testPreparedPaths();
    paths.throughput.fetch.protocol = "http2";
    return { id: String(i), paths };
  });
  const config = {
    ...structuredClone(DEFAULT_CONFIG),
    transferStreams: { mode: "forced" as const, count: 33 },
  };
  expect(() =>
    planServerStreams(config, paths, {
      stage: "download",
      transfer: ["down"],
      loadedLatency: false,
    }),
  ).toThrow("128 streams");
});
