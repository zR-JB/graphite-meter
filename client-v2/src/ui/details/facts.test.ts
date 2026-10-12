import { expect, test } from "bun:test";
import { classify } from "../../engine/net/paths";
import type { MultiServerResult } from "../../engine/run/result";
import type { Discovery } from "../../engine/types";
import type { Preflight } from "../../protocol/documents";
import { capabilities, evidence, httpPaths, loadSummary, serverIssues } from "./facts";

const page = { origin: "https://ui.example", secure: true, webTransport: "available" as const };
function offer(throughput: object[], latency: object[]): Discovery {
  return {
    generation: "g1",
    engineVersion: "",
    server: { name: "" },
    uploadCheckpoint: true,
    maxStageMs: 300_000,
    origin: page.origin,
    protocol: null,
    readAt: 0,
    requestMs: 0,
    ...classify({ throughput, latency } as Preflight["capabilities"], page.origin, page),
  };
}

test("load reads as slots, busy past half of them, and not at all without a limit", () => {
  const cases: [Parameters<typeof loadSummary>[0], string | null][] = [
    [{ active: 0, max: 4 }, "0 of 4 slots"],
    [{ active: 1, max: 2 }, "1 of 2 slots"],
    [{ active: 3, max: 4 }, "3 of 4 slots, server busy: results may be affected"],
    [{ active: 0, max: 0 }, null],
    [undefined, null],
  ];
  for (const [load, text] of cases) expect(loadSummary(load)).toBe(text);
});

test("each side's HTTP version reads on its own", () => {
  expect(evidence("h2", "http/1.1", "Pending")).toBe("Browser HTTP/2, server HTTP/1.1");
  expect(evidence(null, "h3", "Pending")).toBe("Server HTTP/3");
  expect(evidence("h3-29", null, "Pending")).toBe("Browser h3-29");
  expect(evidence(null, null, "—")).toBe("—");
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
  expect(capabilities(discovery, "throughput")).toEqual(["Fetch streams", "Some clear origins blocked by this page"]);
  expect(capabilities(discovery, "latency")).toEqual(["WebTransport datagrams"]);
  expect(httpPaths(null)).toBeNull();
});

test("a server's issues name the stages each reason ended", () => {
  const failure = (stage: string, scope: string, reason: string, serverId = "a") => ({
    serverId,
    stage,
    scope,
    reason,
  });
  const details = {
    failures: [
      failure("download", "throughput", "timeout"),
      failure("upload", "latency", "timeout"),
      failure("latency", "latency", "connection-lost"),
      failure("upload", "throughput", "timeout", "b"),
    ],
  } as unknown as MultiServerResult;
  expect(serverIssues(details, "a")).toEqual([
    { stages: "Download, Loaded up", reason: "Stopped delivering data" },
    { stages: "Latency", reason: "Connection lost" },
  ]);
});
