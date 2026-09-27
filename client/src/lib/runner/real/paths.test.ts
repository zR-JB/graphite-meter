import { test, expect } from "bun:test";
import {
  laneStaggerMs,
  protocolFromNextHop,
  selectTarget,
  browserProtocolMatchesTarget,
  classifyTransportDiscovery,
  fetchViewOfOrigin,
  candidates,
  type AnyTarget,
} from "../paths";
import type { DiscoveredTarget } from "../contract";
import { isLoopbackHostname } from "../../servers/catalog";
import { testTransfer } from "../test-helpers.testutil";
import {
  dgAd,
  fetchAd,
  wsAd,
  wtAd,
  wtLatencyAd,
} from "./test-helpers.testutil";

const targetOfKind = <T extends AnyTarget>(
  entry: DiscoveredTarget<T> | undefined,
  kind: string,
) => entry?.targets.find((target) => target.transport === kind);
type Advertised = Parameters<typeof classifyTransportDiscovery>;
const discovery = (
  throughput: Advertised[0],
  latency: Advertised[1] = [],
  pageOrigin = "http://meter:7246",
  pageSecure = false,
  pageProtocol = "http/1.1",
) =>
  classifyTransportDiscovery(
    throughput,
    latency,
    pageOrigin,
    pageSecure,
    pageProtocol,
  );

test("proxy endpoints resolve relative to preflight and negotiate the browser hop", () => {
  const catalog = discovery(
    [{ baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" }],
    [{ baseUrl: ".", transport: "websocket" }],
    "https://meter.example",
    true,
    "h2",
  );
  const target = selectTarget(catalog, "throughput", "auto", true);
  expect(target?.origin).toBe("https://meter.example");
  expect(target?.protocol).toBe("negotiated");
  if (target?.transport !== "fetch-stream") throw new Error("not fetch");
  expect(browserProtocolMatchesTarget(target, "h2")).toBe(true);
  expect(selectTarget(catalog, "latency", "auto", false)?.origin).toBe(
    "https://meter.example",
  );
});

test("Automatic prefers HTTP1 bulk streams deterministically and never selects unreliable throughput", () => {
  const offered = [
    fetchAd("https://meter", "http1"),
    fetchAd("https://meter:2", "http2"),
    fetchAd("https://meter:3", "http3"),
    wtAd("https://meter:3"),
    dgAd("https://meter:3"),
  ];
  const catalog = discovery(
    offered,
    [wsAd("https://meter"), wtLatencyAd("https://meter:3")],
    "https://meter",
    true,
  );
  const ids = candidates(catalog, "throughput", true).map(
    (target) => target.id,
  );
  expect(ids).toEqual([
    "https://meter",
    "https://meter:2",
    "https://meter:3",
    "https://meter:3::wt",
  ]);
  expect(
    candidates(
      discovery(offered.toReversed(), [], "https://meter", true),
      "throughput",
      true,
    ).map((target) => target.id),
  ).toEqual(ids);
  expect(
    candidates(catalog, "latency", true).map((target) => target.transport),
  ).toEqual(["webtransport", "websocket"]);
  expect(
    candidates(catalog, "latency", false).map((target) => target.transport),
  ).toEqual(["websocket"]);
});

test("multiple off-origin alternatives remain usable and a proven proxy hop participates in ranking", () => {
  const catalog = discovery(
    [
      fetchAd("https://b:3", "http3"),
      fetchAd("https://a:3", "http3"),
      fetchAd(".", "negotiated"),
    ],
    [],
    "https://proxy",
    true,
    "h2",
  );
  expect(selectTarget(catalog, "throughput", "auto", true)?.origin).toBe(
    "https://proxy",
  );
  catalog.pageProtocol = "h3";
  expect(selectTarget(catalog, "throughput", "auto", true)?.origin).toBe(
    "https://proxy",
  );
});

test("deterministic native target wins when self resolves to the same origin", () => {
  const catalog = discovery(
    [fetchAd("https://meter.example", "http1"), fetchAd(".", "negotiated")],
    [],
    "https://meter.example",
    true,
    "http/1.1",
  );
  expect(
    targetOfKind(catalog.throughput["https://meter.example"], "fetch-stream")
      ?.protocol,
  ).toBe("http1");
  expect(selectTarget(catalog, "throughput", "auto", true)?.protocol).toBe(
    "http1",
  );
});

test("native endpoints remain deterministic and mixed content stays blocked", () => {
  const catalog = discovery(
    [
      fetchAd("http://meter:7246", "http1"),
      fetchAd("https://meter:7248", "http2"),
    ],
    [],
    "https://ui.example",
    true,
    "h2",
  );
  expect(catalog.throughput["http://meter:7246"].state).toBe("browser-blocked");
  expect(
    selectTarget(catalog, "throughput", "https://meter:7248", true)?.protocol,
  ).toBe("http2");
  const h2Target = selectTarget(
    catalog,
    "throughput",
    "https://meter:7248",
    true,
  );
  if (h2Target?.transport !== "fetch-stream") throw new Error("not fetch");
  expect(browserProtocolMatchesTarget(h2Target, "http/1.1")).toBe(false);
});

test("an IPv6 origin resolves each of its mechanisms", () => {
  const origin = "https://[2001:db8::1]:7249";
  const catalog = discovery(
    [fetchAd(origin), wtAd(origin)],
    [wtLatencyAd(origin)],
    origin,
    true,
    "h3",
  );
  expect(selectTarget(catalog, "throughput", origin, true)?.transport).toBe(
    "fetch-stream",
  );
  expect(
    selectTarget(catalog, "throughput", `${origin}::wt`, true)?.transport,
  ).toBe("webtransport");
  expect(
    selectTarget(catalog, "latency", `${origin}::wt`, true)?.transport,
  ).toBe("webtransport");
  expect(selectTarget(catalog, "latency", origin, true)?.transport).toBe(
    "webtransport",
  );
});

test("WebTransport folds onto its origin and leads latency auto-selection", () => {
  const catalog = discovery(
    [
      fetchAd("https://meter:7249"),
      wtAd("https://meter:7249"),
      dgAd("https://meter:7249"),
    ],
    [wsAd("https://meter:7247"), wtLatencyAd("https://meter:7249")],
    "https://meter:7249",
    true,
    "h3",
  );
  const entry = catalog.throughput["https://meter:7249"];
  const wtStreams = targetOfKind(entry, "webtransport");
  const datagram = targetOfKind(entry, "webtransport-datagram");
  expect(targetOfKind(entry, "fetch-stream")?.transport).toBe("fetch-stream");
  expect(wtStreams?.id).toBe("https://meter:7249::wt");
  expect(datagram?.id).toBe("https://meter:7249::wtdg");
  expect(datagram?.transport).toBe("webtransport-datagram");
  expect(selectTarget(catalog, "throughput", "auto", true)?.transport).toBe(
    "fetch-stream",
  );
  expect(
    selectTarget(catalog, "throughput", "https://meter:7249::wt", true)
      ?.transport,
  ).toBe("webtransport");
  expect(
    selectTarget(catalog, "throughput", "https://meter:7249::wtdg", true)
      ?.transport,
  ).toBe("webtransport-datagram");
  expect(
    selectTarget(catalog, "throughput", "https://meter:7249", true)?.transport,
  ).toBe("fetch-stream");
  expect(selectTarget(catalog, "latency", "auto", false)?.transport).toBe(
    "websocket",
  );
  expect(
    selectTarget(catalog, "latency", "https://meter:7249", false),
  ).toBeNull();
  const wt = selectTarget(catalog, "latency", "auto", true);
  expect(wt?.transport).toBe("webtransport");
  expect(wt?.origin).toBe("https://meter:7249");
  const both = discovery(
    [fetchAd("https://meter")],
    [wsAd("https://meter"), wtLatencyAd("https://meter")],
    "https://meter",
    true,
    "h3",
  );
  expect(targetOfKind(both.latency["https://meter"], "webtransport")?.id).toBe(
    "https://meter::wt",
  );
  for (const [selection, webTransport, transport] of [
    ["auto", true, "webtransport"],
    ["auto", false, "websocket"],
    ["https://meter::wt", true, "webtransport"],
    ["https://meter", false, "websocket"],
  ] as const)
    expect(
      selectTarget(both, "latency", selection, webTransport)?.transport,
    ).toBe(transport);
});

test("a WebTransport-only origin is auto's last resort and keeps a fetch view", () => {
  const catalog = discovery(
    [wtAd("https://meter:7249")],
    [],
    "https://ui.example",
    true,
    "h3",
  );
  const target = selectTarget(catalog, "throughput", "auto", true);
  expect(target?.transport).toBe("webtransport");
  if (target?.transport !== "webtransport") throw new Error("not wt");
  const view = fetchViewOfOrigin(catalog, target);
  expect(view.transport).toBe("fetch-stream");
  expect(view.origin).toBe("https://meter:7249");
});

test("an explicit WebSocket target resolves to a WebSocket bus", () => {
  const catalog = discovery(
    [fetchAd(".", "negotiated")],
    [{ baseUrl: ".", transport: "websocket" }],
    "https://meter.test",
    true,
  );
  expect(selectTarget(catalog, "latency", "auto", false)?.transport).toBe(
    "websocket",
  );
});

test("browser protocol verification is independent of server probe evidence", () => {
  const h1 = testTransfer("http1-tls", "https://meter", "http1", true);
  const h2 = testTransfer("http2", "https://meter", "http2", true);
  const negotiated = testTransfer(
    "https://meter",
    "https://meter",
    "negotiated",
    true,
  );
  expect(browserProtocolMatchesTarget(h1, "http/1.1")).toBe(true);
  expect(browserProtocolMatchesTarget(h2, "h2")).toBe(true);
  expect(browserProtocolMatchesTarget(h2, "http/1.1")).toBe(false);
  expect(browserProtocolMatchesTarget(negotiated)).toBe(true);
  expect(browserProtocolMatchesTarget(h2)).toBe(false);
  expect(protocolFromNextHop()).toBeUndefined();
  expect(protocolFromNextHop("h2")).toBe("http2");
});

test("clear DNS and IPv4 loopback targets stay usable from HTTPS", () => {
  for (const host of ["localhost", "meter.localhost", "127.42.0.9"]) {
    const target = testTransfer(
      "http1-clear",
      `http://${host}:7246`,
      "http1",
      false,
    );
    expect(
      discovery([target], [], "https://ui.example", true).throughput[
        target.origin
      ].state,
    ).toBe("advertised");
  }
  expect(isLoopbackHostname("127.255.1.2")).toBe(true);
  expect(isLoopbackHostname("[::1]")).toBe(true);
});

test("lane stagger splits half the warmup across later lanes, capped at its base", () => {
  for (const [lanes, warmupMs, baseMs, expected] of [
    [1, 4000, 75, 0],
    [4, 0, 75, 0],
    [4, 3000, 500, 500],
    [2, 100_000, 75, 75],
  ])
    expect(laneStaggerMs(lanes, warmupMs, baseMs)).toBe(expected);
});
