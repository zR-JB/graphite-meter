import { expect, test } from "bun:test";
import {
  buildCardGraphs,
  cardFacts,
  cardNoData,
  summaryCards,
  summaryEvidence,
  type SummaryCard,
} from "./resultSummary";
import { JARGON } from "./vocabulary";

import type { LatencyBucket, ThroughputSample } from "../runner/contract";
import {
  compactThroughputHistory,
  singleLatencyBucket,
  upsertLatencyBucket,
} from "../runner/series";
import { replies } from "./stageGraph";

test("card graphs match full reconstruction across revisions, compaction, and resets", () => {
  let throughput: ThroughputSample[] = [];
  let latency: LatencyBucket[] = [];
  const spans = { download: 1000, upload: 2000, bidirectional: 3000 };
  let previous = buildCardGraphs(throughput, latency, spans);
  const check = () => {
    const snapshot = structuredClone(previous);
    const next = buildCardGraphs(throughput, latency, spans, previous);
    for (const key of ["download", "upload", "bidirectional"] as const) {
      const lane = (dir: "down" | "up") =>
        throughput
          .filter((s) => s.phase === key && s.dir === dir)
          .map((s) => ({ t: s.t, v: s.bytesPerSec }));
      const lanes =
        key === "bidirectional"
          ? [lane("down"), lane("up")]
          : [lane(key === "download" ? "down" : "up")];
      const times = lanes.flat().map((point) => point.t);
      const start = times.length ? Math.min(...times) : 0;
      expect(next[key]).toEqual({
        lanes,
        latency: replies(latency, key),
        start,
        span:
          Math.max(times.length ? Math.max(...times) - start : 0, spans[key]) ||
          1,
      });
    }
    expect(previous).toEqual(snapshot);
    previous = next;
    return next;
  };
  for (let t = 0; t < 24; t++) {
    for (const phase of ["download", "upload", "bidirectional"] as const)
      for (const dir of ["down", "up"] as const)
        throughput.push({
          t: t * 100,
          phase,
          dir,
          bytesPerSec: (t % 5) * 10,
          bytesCumulative: t * 1000,
          continuityId: 0,
        });
    upsertLatencyBucket(
      latency,
      singleLatencyBucket(t * 100, t, t % 3 === 0, "download"),
    );
  }
  check();
  const settled = previous.download;
  expect(check().download).toBe(settled);
  throughput.push({
    t: 2500,
    phase: "upload",
    dir: "up",
    bytesPerSec: 90,
    bytesCumulative: 9000,
    continuityId: 0,
  });
  expect(check().download).toBe(settled);
  throughput[4] = { ...throughput[4], bytesPerSec: 123 };
  upsertLatencyBucket(latency, singleLatencyBucket(400, 75, false, "download"));
  check();
  compactThroughputHistory(throughput, 3000, 12);
  upsertLatencyBucket(
    latency,
    singleLatencyBucket(2600, 90, false, "download"),
    5,
  );
  check();
  spans.download = spans.upload = spans.bidirectional = 0;
  check();
  throughput = [];
  latency = [];
  check();
  expect(previous.download).toEqual({
    lanes: [[]],
    latency: [],
    start: 0,
    span: 1,
  });
});

test("run evidence keeps only stages with a result", () => {
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
  );
  expect(evidence.status).toEqual({ download: "complete", upload: "failed" });
});

const lane = (reportedBytesPerSec: number) => ({
  reportedBytesPerSec,
  totalBytes: 1_000_000,
  stabilityPct: 95,
  peakBytesPerSec: null,
});
const units = { base: "base10", kind: "bytes" } as const;

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
      units,
      show,
    )[0];
  expect(wire(1.004).wire).toBeUndefined();
  expect(wire(1.05).wire).toMatchObject({
    value: "105.0 B/s",
    overhead: "+5.0%",
  });
  expect(wire(1.05, false).wire).toBeUndefined();
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
    units,
    true,
  );
  expect(card.num).toBe("—");
  expect(card.rows.map((row) => row.value)).toEqual([
    "40.00 B/s",
    "unavailable",
  ]);
});

test("the latency card groups signed added latency, even of a failed stage; transfer cards show stability as a value", () => {
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
      latency: { reportedMs: 12, jitterMs: 1, stabilityPct: 91.7 },
      added: { download: 8.25, upload: -0.04, bidirectional: 0 },
    },
    units,
    true,
  );
  const added = cards[0].rows.filter((row) => row.label === "Added");
  expect(added.map((row) => [row.stage, row.value])).toEqual([
    ["download", "+8.3 ms"],
    ["upload", "+0.0 ms"],
    ["bidirectional", "+0.0 ms"],
  ]);
  const stability = (card: (typeof cards)[number]) =>
    card.rows.find((row) => row.label === "Stability")?.value;
  expect(cards.slice(1).map(stability)).toEqual(["95%", "80%", undefined]);
});

test("time without data is an explained fact from half a second, the longer lane's on bidirectional", () => {
  const noData = (
    download: number | undefined,
    lanes: [number, number] = [0, 0],
  ) =>
    summaryCards(
      {
        status: { download: "complete", bidirectional: "complete" },
        download: { ...lane(100), quietMs: download },
        upload: null,
        bidirectional: {
          down: { ...lane(40), quietMs: lanes[0] },
          up: { ...lane(20), quietMs: lanes[1] },
        },
        latency: null,
        added: null,
      },
      units,
      true,
    ).map((card) => card.rows.find((row) => row.label === "No data"));
  expect(noData(undefined)).toEqual([undefined, undefined]);
  expect(noData(499, [0, 499])).toEqual([undefined, undefined]);
  const fact = (value: string) => ({
    label: "No data",
    value,
    tip: JARGON.noData,
  });
  expect(noData(8_000, [600, 2_500])).toEqual([fact("8.0 s"), fact("2.5 s")]);
});

test("a transfer card keeps the same facts in every state, a dash until known; No data joins its line", () => {
  const facts = (card: SummaryCard) =>
    cardFacts(card).map((row) => `${row.label} ${row.value}`);
  const waiting: SummaryCard = {
    key: "download",
    label: "Down",
    icon: "download",
    status: "pending",
    num: "—",
    unit: "",
    tip: "",
    rows: [],
  };
  expect(facts(waiting)).toEqual(["Peak —", "Stability —", "Transferred —"]);
  const running = [{ label: "Transferred", value: "1.0 MB" }];
  expect(facts({ ...waiting, status: "active", rows: running })).toEqual([
    "Peak —",
    "Stability —",
    "Transferred 1.0 MB",
  ]);
  const [download, bidirectional] = summaryCards(
    {
      status: { download: "complete", bidirectional: "complete" },
      download: { ...lane(100), peakBytesPerSec: 120, quietMs: 800 },
      upload: null,
      bidirectional: { down: lane(40), up: lane(20) },
      latency: null,
      added: null,
    },
    units,
    true,
  );
  expect(facts(download)).toEqual([
    "Peak 120.0 B/s",
    "Stability 95%",
    "Transferred 1.0 MB",
  ]);
  expect(cardNoData(download)?.value).toBe("0.8 s");
  expect(cardNoData(bidirectional)).toBeNull();
  expect(facts(bidirectional)).toEqual([
    "Stability 95%",
    "Down + up 60.00 B/s",
    "Transferred 2.0 MB",
  ]);
});
