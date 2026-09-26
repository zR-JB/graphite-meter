import { expect, test } from "bun:test";
import type { MultiServerResult } from "../runner/measure";
import {
  liveWire,
  serverEvidence,
  summaryCards,
  summaryEvidence,
  wireOverhead,
} from "./resultSummary";

test("run evidence keeps only stages with a result and names no single source", () => {
  const evidence = summaryEvidence(
    {
      latency: "not-run",
      download: "complete",
      upload: "failed",
      bidirectional: "active",
    },
    {
      download: null,
      upload: null,
      bidirectional: null,
      latency: null,
      added: null,
      wire: {},
    },
    null,
    "",
    "self",
  );
  expect(evidence.status).toEqual({ download: "complete", upload: "failed" });
  expect(evidence.latencySource).toBeUndefined();
});

test("live wire overhead appears from half a percent", () => {
  const estimate = { totalMultiplier: 1.004 } as Parameters<typeof liveWire>[0];
  expect(liveWire(estimate)).toBeNull();
  expect(wireOverhead(1.05)).toBe("+5.0%");
});

const lane = (reportedBytesPerSec: number) => ({
  reportedBytesPerSec,
  totalBytes: 1_000_000,
  stabilityPct: 95,
});
const rate = (value: number) => ({ num: String(value), unit: "B/s" });

test("a one-lane bidirectional result has no combined value, only its surviving lane", () => {
  const [card] = summaryCards(
    {
      status: { bidirectional: "partial" },
      download: null,
      upload: null,
      bidirectional: { down: lane(40), up: null },
      latency: null,
      added: null,
      latencyMeasured: true,
      wire: {},
    },
    rate,
    "base10",
  );
  expect(card).toMatchObject({
    num: "—",
    quality: null,
    detail: "↓ 40 B/s · upload unavailable",
  });
});

test("a server's own failures decide its statuses; a server without a latency path is unmeasured", () => {
  const server = (id: string, latency: boolean) => ({
    server: { id, name: id, url: `https://${id}.test` },
    throughput: { origin: "", transport: "fetch-stream", protocol: "http1" },
    latencyTarget: latency ? { origin: "", transport: "websocket" } : null,
    latency: null,
    latencyByStage: {
      latency: null,
      download: null,
      upload: null,
      bidirectional: null,
    },
    bufferbloat: null,
    download: lane(10) as never,
    upload: null,
    bidirectional: null,
    totalBytes: { down: 1, up: 0 },
  });
  const details = {
    servers: [server("a", true), server("b", false)],
    failures: [
      {
        serverId: "a",
        stage: "upload",
        scope: "throughput",
        atMs: 0,
        reason: "",
        message: "",
      },
    ],
  } as unknown as MultiServerResult;
  const status = {
    download: "complete",
    upload: "complete",
    latency: "complete",
  } as const;
  expect(serverEvidence(details, "a", status)?.status).toEqual({
    latency: "partial",
    download: "complete",
    upload: "failed",
  });
  const b = serverEvidence(details, "b", status)!;
  expect(b.status).toEqual({
    latency: "complete",
    download: "complete",
    upload: "partial",
  });
  expect(summaryCards(b, rate, "base10").at(-1)).toMatchObject({
    num: "—",
    detail: "Not measured",
  });
});

test("cards show signed added latency, the grade, and one pip rule", () => {
  const cards = summaryCards(
    {
      status: { download: "complete", upload: "complete", latency: "complete" },
      download: lane(40),
      upload: { ...lane(20), stabilityPct: 80 },
      bidirectional: null,
      latency: { reportedMs: 12, jitterMs: 1 },
      added: { addedMs: { download: 8.25, upload: -0.04 }, grade: "B" },
      latencyMeasured: true,
      wire: {},
    },
    rate,
    "base10",
  );
  const shown = cards.map((card) => [
    card.added,
    card.grade,
    card.quality?.band,
  ]);
  expect(shown).toEqual([
    ["+8.3", null, "high"],
    ["0.0", null, "medium"],
    [null, "Grade B", "high"],
  ]);
});

test("records saved before per-stage added latency show only the grade", () => {
  const [latency] = summaryCards(
    {
      status: { latency: "complete" },
      download: null,
      upload: null,
      bidirectional: null,
      latency: { reportedMs: 12, jitterMs: 1 },
      added: { grade: "C" },
      latencyMeasured: true,
      wire: {},
    },
    rate,
    "base10",
  );
  expect(latency.grade).toBe("Grade C");
});
