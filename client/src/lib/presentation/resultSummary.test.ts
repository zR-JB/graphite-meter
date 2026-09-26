import { expect, test } from "bun:test";
import { summaryCards, summaryEvidence } from "./resultSummary";

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
    },
    null,
    "",
    "self",
  );
  expect(evidence.status).toEqual({ download: "complete", upload: "failed" });
  expect(evidence.latencySource).toBeUndefined();
});

const lane = (reportedBytesPerSec: number) => ({
  reportedBytesPerSec,
  totalBytes: 1_000_000,
  stabilityPct: 95,
});
const rate = (value: number) => ({ num: String(value), unit: "B/s" });

test("a saved wire overhead shows from half a percent and only when chosen", () => {
  const wire = (totalMultiplier: number, show = true) =>
    summaryCards(
      {
        status: { download: "complete" },
        download: {
          ...lane(100),
          wire: {
            factors: [],
            transport: "http2",
            transportSource: "detected",
            framing: null,
            mtuBytes: 1500,
            ipVersion: 4,
            ipVersionSource: "detected",
            totalMultiplier,
          },
        },
        upload: null,
        bidirectional: null,
        latency: null,
        added: null,
      },
      rate,
      "base10",
      show,
    )[0].wire;
  expect(wire(1.004)).toBeNull();
  expect(wire(1.05)).toMatchObject({ pct: "+5.0%", num: "105" });
  expect(wire(1.05, false)).toBeNull();
});

test("a one-lane bidirectional result has no combined value, only its surviving lane", () => {
  const [card] = summaryCards(
    {
      status: { bidirectional: "partial" },
      download: null,
      upload: null,
      bidirectional: { down: lane(40), up: null },
      latency: null,
      added: null,
    },
    rate,
    "base10",
    true,
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
      status: {
        download: "complete",
        upload: "complete",
        bidirectional: "failed",
        latency: "complete",
      },
      download: lane(40),
      upload: { ...lane(20), stabilityPct: 80 },
      bidirectional: null,
      latency: { reportedMs: 12, jitterMs: 1 },
      added: {
        addedMs: { download: 8.25, upload: -0.04, bidirectional: 0 },
        grade: "B",
      },
    },
    rate,
    "base10",
    true,
  );
  const shown = cards.map((card) => [
    card.added,
    card.grade,
    card.quality?.band,
  ]);
  expect(shown).toEqual([
    ["+8.3", null, "high"],
    ["+0.0", null, "medium"],
    [null, null, undefined],
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
    },
    rate,
    "base10",
    true,
  );
  expect(latency.grade).toBe("Grade C");
});
