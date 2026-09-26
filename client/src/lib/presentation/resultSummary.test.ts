import { expect, test } from "bun:test";
import {
  liveWire,
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

test("cards show signed added latency, the grade, and one pip rule", () => {
  const cards = summaryCards(
    {
      status: { download: "complete", upload: "complete", latency: "complete" },
      download: lane(40),
      upload: { ...lane(20), stabilityPct: 80 },
      bidirectional: null,
      latency: { reportedMs: 12, jitterMs: 1 },
      added: { addedMs: { download: 8.25, upload: -0.04 }, grade: "B" },
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
    ["+0.0", null, "medium"],
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
      wire: {},
    },
    rate,
    "base10",
  );
  expect(latency.grade).toBe("Grade C");
});
