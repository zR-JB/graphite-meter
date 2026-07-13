import { test, expect } from "bun:test";
import {
  resolveBase,
  httpToWs,
  wsToWss,
  median,
  needsPings,
  laneStaggerMs,
  selectProtocolTarget,
  browserProtocolMatchesTarget,
  protocolTargetKey,
} from "./real/backendPure";
import type { PhaseActivity } from "./contract";

const routes = {
  probe: "/probe",
  download: "/download",
  upload: "/upload",
  uploadSession: "/upload/session",
  websocket: null,
  webtransport: null,
};

test("selectProtocolTarget freezes the requested advertised target", () => {
  const targets = {
    http1: { origin: "http://meter:8765", routes },
    http2: { origin: "https://meter:8443", routes },
    http3: null,
  };
  expect(
    selectProtocolTarget(targets, "http2", "http://meter:8765", false)
      ?.protocol,
  ).toBe("http2");
  expect(
    selectProtocolTarget(targets, "http3", "http://meter:8765", false),
  ).toBeNull();
});

test("selectProtocolTarget rejects clear H1 on a secure page", () => {
  const targets = {
    http1: { origin: "http://meter:8765", routes },
    http2: null,
    http3: null,
  };
  expect(
    selectProtocolTarget(targets, "http1", "https://meter", true),
  ).toBeNull();
});

test("browser protocol verification is independent of server probe evidence", () => {
  expect(browserProtocolMatchesTarget("http1", "http/1.1")).toBe(true);
  expect(browserProtocolMatchesTarget("http2", "h2")).toBe(true);
  expect(browserProtocolMatchesTarget("http3", "h3")).toBe(true);
  expect(browserProtocolMatchesTarget("http2", "http/1.1")).toBe(false);
});

test("idle target ownership includes protocol and public origin", () => {
  const target = { origin: "https://meter", routes };
  expect(protocolTargetKey("http2", target)).toBe("http2\nhttps://meter");
  expect(protocolTargetKey("http3", target)).not.toBe(
    protocolTargetKey("http2", target),
  );
  expect(
    protocolTargetKey("http2", { ...target, origin: "https://other-meter" }),
  ).not.toBe(protocolTargetKey("http2", target));
});

/* ---------- resolveBase ---------- */

test("resolveBase: undefined/auto/empty host all mean same-origin (relative)", () => {
  expect(resolveBase(undefined)).toBe("");
  expect(resolveBase({ host: "auto", port: 8765 })).toBe("");
  expect(resolveBase({ host: "", port: 8765 })).toBe("");
});

test("resolveBase: builds an absolute http origin for a concrete host", () => {
  expect(resolveBase({ host: "example.com", port: 8765 })).toBe(
    "http://example.com:8765",
  );
});

test("resolveBase: port 443 builds an https origin", () => {
  expect(resolveBase({ host: "example.com", port: 443 })).toBe(
    "https://example.com:443",
  );
});

/* ---------- httpToWs ---------- */

test("httpToWs: maps https:// to wss:// and http:// to ws://", () => {
  expect(httpToWs("https://example.com:443")).toBe("wss://example.com:443");
  expect(httpToWs("http://example.com:8765")).toBe("ws://example.com:8765");
});

test("httpToWs: passes through anything already ws(s):// or relative", () => {
  expect(httpToWs("wss://example.com")).toBe("wss://example.com");
  expect(httpToWs("ws://example.com")).toBe("ws://example.com");
  expect(httpToWs("")).toBe("");
});

/* ---------- wsToWss ---------- */

test("wsToWss: upgrades ws:// to wss://", () => {
  expect(wsToWss("ws://example.com:8765")).toBe("wss://example.com:8765");
});

test("wsToWss: leaves wss:// (or anything else) unchanged", () => {
  expect(wsToWss("wss://example.com:443")).toBe("wss://example.com:443");
  expect(wsToWss("")).toBe("");
});

/* ---------- median ---------- */

test("median: odd-length list returns the middle value", () => {
  expect(median([3, 1, 2])).toBe(2);
});

test("median: even-length list averages the two middle values", () => {
  expect(median([4, 1, 3, 2])).toBe(2.5);
});

test("median: single-element list returns that element", () => {
  expect(median([7])).toBe(7);
});

test("median: does not mutate the input array", () => {
  const xs = [3, 1, 2];
  median(xs);
  expect(xs).toEqual([3, 1, 2]);
});

/* ---------- needsPings ---------- */

const activity = (overrides: Partial<PhaseActivity> = {}): PhaseActivity => ({
  stage: "download",
  transfer: ["down"],
  loadedLatency: false,
  ...overrides,
});

test("needsPings: the latency stage always needs pings", () => {
  expect(
    needsPings(
      activity({ stage: "latency", transfer: [], loadedLatency: false }),
    ),
  ).toBe(true);
});

test("needsPings: a transfer stage needs pings only when loadedLatency is on", () => {
  expect(needsPings(activity({ loadedLatency: true }))).toBe(true);
  expect(needsPings(activity({ loadedLatency: false }))).toBe(false);
});

test("needsPings: loadedLatency alone is not enough without transfer lanes", () => {
  expect(
    needsPings(
      activity({ transfer: [], loadedLatency: true, stage: "download" }),
    ),
  ).toBe(false);
});

/* ---------- laneStaggerMs ---------- */

test("laneStaggerMs: a single lane (or fewer) never staggers", () => {
  expect(laneStaggerMs(1, 4000, 75)).toBe(0);
  expect(laneStaggerMs(0, 4000, 75)).toBe(0);
});

test("laneStaggerMs: zero warmup means lanes spawn together", () => {
  expect(laneStaggerMs(4, 0, 75)).toBe(0);
});

test("laneStaggerMs: splits half the warmup window across the non-first lanes", () => {
  // 4 lanes -> 3 gaps; half of a 3000ms warmup (1500ms) split 3 ways = 500ms/gap.
  expect(laneStaggerMs(4, 3000, 500)).toBe(500);
});

test("laneStaggerMs: caps at the base stagger even on a long warmup", () => {
  // A generous warmup shouldn't stretch the per-lane gap past the base.
  expect(laneStaggerMs(2, 100_000, 75)).toBe(75);
});
