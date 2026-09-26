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
  peakBytesPerSec: null,
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
    )[0];
  const face = (card: ReturnType<typeof wire>) =>
    card.rows.find((row) => row.label === "Wire")?.value;
  expect(face(wire(1.004))).toBeUndefined();
  expect(face(wire(1.05))).toBe("105 B/s");
  expect(wire(1.05).details.at(-1)?.value).toBe("+5.0%");
  expect(face(wire(1.05, false))).toBeUndefined();
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
  expect(card.num).toBe("—");
  expect(card.details).toEqual([]);
  expect(card.rows.map((row) => row.value)).toEqual(["40 B/s", "unavailable"]);
});

test("the latency card groups signed added latency; details show stability as a value", () => {
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
      added: { download: 8.25, upload: -0.04, bidirectional: 0 },
    },
    rate,
    "base10",
    true,
  );
  const added = cards.at(-1)!.rows.filter((row) => row.label === "Added");
  expect(added.map((row) => [row.stage, row.value])).toEqual([
    ["download", "+8.3 ms"],
    ["upload", "+0.0 ms"],
  ]);
  const stability = (card: (typeof cards)[number]) =>
    card.details.find((row) => row.label === "Stability")?.value;
  expect(cards.map(stability)).toEqual(["95%", "80%", undefined, "92%"]);
});
