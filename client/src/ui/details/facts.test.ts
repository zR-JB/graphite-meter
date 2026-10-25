import { expect, test } from "bun:test";
import { classify } from "../../engine/net/paths";
import type { Discovery } from "../../engine/types";
import type { Preflight } from "../../protocol/documents";
import { MISSING } from "../../view/format";
import { capabilities, evidence, httpPaths, loadSummary } from "./facts";

const page = { origin: "https://ui.example", secure: true, webTransport: "available" as const };
function offer(throughput: object[], latency: object[]): Discovery {
  return {
    generation: "g1",
    engineVersion: "",
    server: { name: "" },
    maxStageMs: 300_000,
    maxMeasurements: 32,
    origin: page.origin,
    protocol: null,
    readAt: 0,
    requestMs: 0,
    ...classify({ throughput, latency } as Preflight["capabilities"], page.origin, page),
  };
}

test("load reads busy only past half the slots, and not at all without a limit", () => {
  const busy = (active: number, max: number) => loadSummary({ active, max })?.includes("busy");
  expect([busy(0, 4), busy(1, 2), busy(3, 4)]).toEqual([false, false, true]);
  expect(loadSummary({ active: 0, max: 0 })).toBeNull();
  expect(loadSummary(undefined)).toBeNull();
});

test("each side's HTTP version reads on its own, unknown until either reports", () => {
  const both = evidence("h2", "http/1.1", MISSING);
  expect(both.indexOf("HTTP/2")).toBeLessThan(both.indexOf("HTTP/1.1"));
  expect(evidence(null, "h3", MISSING)).toContain("HTTP/3");
  expect(evidence(null, null, MISSING)).toBe(MISSING);
});

test("a server supports what it advertises; latency targets are no HTTP paths", () => {
  const discovery = offer(
    [
      { baseUrl: ".", transport: "fetch-stream", protocol: "http2" },
      { baseUrl: "http://clear.example", transport: "fetch-stream", protocol: "http1" },
    ],
    [{ baseUrl: ".", transport: "webtransport" }],
  );
  expect(httpPaths(discovery)).toEqual(["HTTP/2 (TLS)", "HTTP/1.1 (clear)"]);
  expect(capabilities(discovery, "throughput")).toEqual([
    "Fetch streams",
    "Some clear origins blocked by this page",
  ]);
  expect(capabilities(discovery, "latency")).toEqual(["WebTransport datagrams"]);
  expect(httpPaths(null)).toBeNull();
});
