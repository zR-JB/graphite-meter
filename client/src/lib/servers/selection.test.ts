import { expect, test } from "bun:test";
import {
  classifyTransportDiscovery,
  planServerStreams,
  portableTransportSelection,
} from "../runner/paths";
import { pathOptions } from "../presentation/paths";
import { DEFAULT_CONFIG } from "../state/defaults";
import { testPreparedPaths } from "../runner/test-helpers.testutil";
import { parseCatalog } from "./catalog";

function configFor(
  role: "throughput" | "latency",
  selected: string,
  datagrams: boolean,
) {
  const config = structuredClone(DEFAULT_CONFIG);
  config.experimentalDatagramThroughput = datagrams;
  config.transports[
    role === "throughput" ? "throughputTarget" : "latencyTarget"
  ] = selected;
  return config;
}
const servers = [
  { id: "a", name: "A", url: "https://a.example" },
  { id: "b", name: "B", url: "https://b.example" },
];
const discoveries = new Map(
  servers.map((server, index) => [
    server.id,
    {
      discovery: classifyTransportDiscovery(
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
    },
  ]),
);
test("automatic may use different reliable protocols while explicit compatibility covers every server", () => {
  const options = pathOptions(
    "throughput",
    servers,
    discoveries,
    configFor("throughput", "auto", false),
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
    pathOptions(
      "latency",
      servers,
      discoveries,
      configFor("latency", "auto", false),
    ).find((option) => option.value === "transport:websocket")?.disabled,
  ).toBe(false);
  expect(
    options.some(
      (option) => option.value === "transport:webtransport-datagram",
    ),
  ).toBe(false);
});
test("path availability ignores a failed server's stale discovery", () => {
  const views = new Map(
    [...discoveries].map(([id, view]) => [
      id,
      { ...view, readiness: id === "b" ? "failed" : "verified" },
    ]),
  );
  const option = pathOptions(
    "throughput",
    servers,
    views,
    configFor("throughput", "auto", false),
  ).find((option) => option.value === "protocol:http1");
  expect(option).toMatchObject({ disabled: false, detail: "Available on A" });
});

test("server transport options name the browser's IPv6 configuration remedy", () => {
  const origin = "http://[::1]:7246";
  const discovery = classifyTransportDiscovery(
    [{ baseUrl: origin, protocol: "http1", transport: "fetch-stream" }],
    [],
    "http://ui.example",
    false,
  );
  const options = pathOptions(
    "throughput",
    [{ id: "ipv6", name: "IPv6 meter", url: origin }],
    new Map([["ipv6", { discovery }]]),
    configFor("throughput", "auto", false),
    undefined,
    true,
  );
  expect(
    options.find((option) => option.value === "protocol:http1"),
  ).toMatchObject({
    disabled: true,
    detail:
      "IPv6 meter: Use a DNS hostname for browser connections to this IPv6 server.",
  });
  expect(options.find((option) => option.value === "auto")?.detail).toContain(
    "DNS hostname",
  );
});

test("shared H1 origins preserve progress and checkpoint capacity", () => {
  const paths = servers.map((server) => ({
    server,
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
  expect(planServerStreams(forced, paths, activity)).toEqual({
    a: { down: 0, up: 3 },
    b: { down: 0, up: 3 },
  });
});
test("a direct H1 upload reserves progress and receiver checkpoint capacity", () => {
  const paths = [
    { server: { id: "self", name: "Home" }, paths: testPreparedPaths() },
  ];
  const config = {
    ...structuredClone(DEFAULT_CONFIG),
    transferStreams: { mode: "forced" as const, count: 4 },
  };
  const activity = {
    stage: "upload" as const,
    transfer: ["up" as const],
    loadedLatency: true,
  };
  expect(
    planServerStreams(
      { ...config, transferStreams: { mode: "auto", count: 4 } },
      paths,
      activity,
    ).self.up,
  ).toBe(3);
  // Forced is exact, even past the browser's connections.
  expect(planServerStreams(config, paths, activity).self.up).toBe(4);
});

test("valid prototype-named server IDs retain their streams", () => {
  const ids = ["constructor", "toString", "__proto__"];
  const catalog = parseCatalog(
    {
      servers: [
        { id: "self", name: "Home", url: "." },
        ...ids.map((id, index) => ({
          id,
          name: id,
          url: `https://server-${index}.example`,
        })),
      ],
      defaultSelection: ids,
    },
    "https://home.example",
  );
  const paths = catalog.servers.slice(1).map((server) => {
    const paths = testPreparedPaths();
    paths.throughput.fetch.protocol = "http2";
    return { server, paths };
  });
  const config = {
    ...structuredClone(DEFAULT_CONFIG),
    transferStreams: { mode: "forced" as const, count: 12 },
  };
  const activity = {
    stage: "download" as const,
    transfer: ["down" as const],
    loadedLatency: false,
  };
  const plan = planServerStreams(config, paths, activity);
  expect(Object.keys(plan)).toEqual(ids);
  for (const id of ids) expect(plan[id]).toEqual({ down: 12, up: 0 });
});

test("switching servers carries a transport preference without the previous origin", () => {
  const { discovery } = discoveries.get("a")!;
  expect(
    portableTransportSelection("throughput", servers[0].url, discovery),
  ).toBe("protocol:http1");
  expect(portableTransportSelection("latency", servers[0].url, discovery)).toBe(
    "transport:websocket",
  );
  expect(
    portableTransportSelection(
      "throughput",
      "https://removed.example",
      discovery,
    ),
  ).toBe("auto");
  expect(
    portableTransportSelection("throughput", "protocol:http3", discovery),
  ).toBe("protocol:http3");
  expect(
    portableTransportSelection("latency", "transport:webtransport", null),
  ).toBe("transport:webtransport");
});

test("saved transport mechanisms remain portable before discovery and never infer an HTTP protocol", () => {
  expect(
    portableTransportSelection("throughput", "https://old.example::wt", null),
  ).toBe("transport:webtransport");
  expect(
    portableTransportSelection("latency", "https://old.example::wt", undefined),
  ).toBe("transport:webtransport");
  expect(
    portableTransportSelection("throughput", "https://old.example::wtdg", null),
  ).toBe("transport:webtransport-datagram");
  expect(
    portableTransportSelection("throughput", "https://old.example", null),
  ).toBe("auto");
});
