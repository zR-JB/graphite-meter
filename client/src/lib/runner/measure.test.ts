import { expect, test } from "bun:test";
import {
  confidenceSampleFloor,
  EARLY_FINISH,
  LatencyPopulation,
  RateBuckets,
  ServerLatency,
  shouldExitPhase,
  stabilityPct,
  ThroughputAggregate,
  transferConfidence,
  type Boundary,
} from "./measure";
import { DURATION_PRESETS } from "../state/defaults";
import { fixedPingIntervalMs } from "./pingCadence";

const reply = (
  rttMs: number,
  timedOut = false,
  rttEligible = true,
  reflectorHandlingMs?: number,
) => ({
  rttMs,
  timedOut,
  observedAtMs: 0,
  rttEligible,
  reflectorHandlingMs,
});
const receiver = (id: string, bytes: number, nanos: number, request = 0) => ({
  id,
  bytes,
  nanos,
  requestedAtMs: request,
  receivedAtMs: request + 7,
});
const boundary = (
  atMs: number,
  down: Record<string, number> = {},
  up: Boundary["up"] = {},
): Boundary => ({
  atMs,
  down,
  up,
});

test("a stage summary keeps the raw distribution, consecutive variation and exact accounting", () => {
  const stats = new LatencyPopulation();
  expect(stats.summary()).toBeNull();
  for (const rtt of [10, 100, 10, 100]) stats.observe(reply(rtt));
  expect(stats.summary()).toEqual({
    accountingComplete: true,
    probeCount: 4,
    timeoutCount: 0,
    unresolvedCount: 0,
    sendFailureCount: 0,
    jitterPairs: 3,
    minMs: 10,
    maxMs: 100,
    meanMs: 55,
    p10Ms: 10,
    p50Ms: 55,
    p90Ms: 100,
    p95Ms: 100,
    jitterMs: 90,
  });
  stats.interrupt(2, "unresolved");
  stats.interrupt(1, "send-failed");
  stats.observe(reply(10_000, true));
  stats.markIncomplete();
  expect(stats.summary()).toMatchObject({
    accountingComplete: false,
    probeCount: 5,
    timeoutCount: 1,
    unresolvedCount: 2,
    sendFailureCount: 1,
    maxMs: 100,
  });
  expect(stats.timeoutRatio).toBe(0.2);
});

test("summaries between replies match one summary of the whole stage", () => {
  const live = new LatencyPopulation();
  const once = new LatencyPopulation();
  let seed = 7;
  for (let i = 1; i <= 2_000; i++) {
    seed = (seed * 48_271) % 2_147_483_647;
    const rtt = (seed % 5_000) / 100;
    live.observe(reply(rtt));
    once.observe(reply(rtt));
    if (i % 37 === 0 || i % 500 === 1) live.summary();
  }
  expect(live.summary()).toEqual(once.summary());
  live.close();
  expect(live.summary()).toEqual(once.summary());
});

test("timeouts skip jitter pairs, interruptions break them, and late replies resolve without RTT", () => {
  const stats = new LatencyPopulation();
  stats.observe(reply(10));
  stats.observe(reply(250, true));
  stats.observe(reply(30));
  stats.observe(reply(1_000), 1);
  stats.observe(reply(1_010), 1);
  stats.observe(reply(900, false, false));
  expect(stats.summary()).toMatchObject({
    jitterMs: 15,
    jitterPairs: 2,
    probeCount: 6,
    maxMs: 1_010,
  });
  const empty = new LatencyPopulation();
  empty.interrupt(3, "unresolved");
  expect(empty.timeoutRatio).toBeNull();
  expect(empty.summary()).toMatchObject({
    probeCount: 0,
    unresolvedCount: 3,
    jitterMs: null,
  });
});

test("paired server timing uses only valid in-window replies and leaves raw statistics unchanged", () => {
  const timed = new LatencyPopulation();
  const raw = new LatencyPopulation();
  const replies: [number, boolean, boolean, number | undefined][] = [
    [10, false, true, 2],
    [20, false, true, 0],
    [30, false, true, undefined],
    [40, false, true, 41],
    [50, false, true, NaN],
    [250, true, true, 200],
    [90, false, false, 30],
  ];
  for (const [rtt, timedOut, eligible, handling] of replies) {
    timed.observe(reply(rtt, timedOut, eligible, handling));
    raw.observe(reply(rtt, timedOut, eligible));
  }
  const { reflectorTiming, ...summary } = timed.summary()!;
  expect(summary).toEqual(raw.summary()!);
  expect(reflectorTiming).toEqual({
    sampleCount: 2,
    meanRawRttMs: 15,
    meanHandlingMs: 1,
  });
});

test("stage populations stay separate and added latency is the signed worst loaded median", () => {
  const latency = new ServerLatency();
  for (const rtt of [10, 20]) latency.observe("latency", reply(rtt), 0, 0);
  for (let i = 0; i < 100; i++) latency.observe("download", reply(20), 0, 0);
  latency.observe("upload", reply(300), 0, 0);
  latency.observe("upload", reply(250, true), 0, 0);
  expect(latency.result()).toMatchObject({ reportedMs: 15 });
  expect(latency.bufferbloat()).toEqual({
    addedMs: { download: 5, upload: 285, bidirectional: null },
    grade: "F",
    idleMs: 15,
    loadedMs: 300,
    increaseMs: 285,
  });
  expect(latency.summaries().bidirectional).toBeNull();
  const faster = new ServerLatency();
  faster.observe("latency", reply(30), 0, 0);
  faster.observe("download", reply(20), 0, 0);
  expect(faster.bufferbloat()).toMatchObject({
    addedMs: { download: -10 },
    grade: "A",
    increaseMs: -10,
  });
  const loadedOnly = new ServerLatency();
  loadedOnly.observe("download", reply(30), 0, 0);
  expect(loadedOnly.result()).toBeNull();
});

test("a failed idle population needs three outcomes and never selects a stable window", () => {
  const latency = new ServerLatency();
  latency.failed.add("latency");
  latency.observe("latency", reply(10), 0, 0);
  latency.observe("latency", reply(20, true), 0, 0);
  expect(latency.result()).toBeNull();
  latency.observe("latency", reply(30), 0, 0);
  expect(latency.result()).toMatchObject({ reportedMs: 20 });
});

test("the idle headline is the full median; stability only labels its band", () => {
  const latency = new ServerLatency();
  for (const rtt of [90, 80, 70, 20, 20])
    latency.observe("latency", reply(rtt), 0, 0);
  latency.trackStable(1);
  expect(latency.result()).toMatchObject({ reportedMs: 70, band: "high" });
});

test("rate buckets split exact byte/time evidence independently of callback chunking", () => {
  const whole = new RateBuckets();
  whole.observe(1_000, 1_000);
  const chunked = new RateBuckets();
  for (let i = 0; i < 10; i++) chunked.observe(100, 100);
  expect(chunked.rates).toEqual(whole.rates);
  expect(whole.rates).toEqual([1_000, 1_000, 1_000, 1_000]);
  expect(stabilityPct(whole.rates)).toBe(100);
});

/** The idle window's confidence after these outcomes; a null RTT is a timeout. */
function latencyConfidence(outcomes: { t: number; rtt: number | null }[]) {
  const latency = new ServerLatency();
  for (const { t, rtt } of outcomes)
    latency.observe("latency", reply(rtt ?? 250, rtt === null), t, 0);
  return latency.confidence();
}

test("confidence scores punish variance, drift, jitter and timeouts", () => {
  expect(transferConfidence([1_000]).score).toBe(0);
  expect(transferConfidence(Array(16).fill(1_000)).score).toBe(1);
  expect(transferConfidence([100, 300, 500, 700, 900, 1_100]).score).toBe(0);
  const steady = (rtt: (i: number) => number | null) =>
    latencyConfidence(
      Array.from({ length: 20 }, (_, i) => ({ t: i * 100, rtt: rtt(i) })),
    );
  expect(steady(() => 20).score).toBe(1);
  expect(steady((i) => (i % 2 ? 20 : 80)).score).toBeLessThan(0.5);
  expect(steady((i) => (i % 4 ? 20 : null)).score).toBeLessThan(0.5);
  // Recovered timeouts age out with the 4 s window.
  const recovered = latencyConfidence(
    Array.from({ length: 80 }, (_, i) => ({
      t: i * 100,
      rtt: i < 10 ? null : 20,
    })),
  );
  expect(recovered.score).toBe(1);
});

test("an early exit needs coverage, a stable score and a feasible evidence floor", () => {
  const exit = (overrides: Partial<Parameters<typeof shouldExitPhase>[0]>) =>
    shouldExitPhase({
      kind: "transfer",
      elapsedMs: 8_000,
      durationMs: 10_000,
      confidence: { score: 0.95, sampleCount: 30 },
      ...overrides,
    });
  expect(exit({})).toBe(true);
  expect(exit({ durationMs: 0 })).toBe(false);
  expect(exit({ elapsedMs: 4_000 })).toBe(false);
  expect(exit({ confidence: { score: 0.5, sampleCount: 30 } })).toBe(false);
  expect(exit({ confidence: { score: 0.95, sampleCount: 5 } })).toBe(false);
  expect(confidenceSampleFloor("transfer", 500)).toBe(4);
  expect(confidenceSampleFloor("latency", 500, "slow")).toBe(3);
  expect(confidenceSampleFloor("latency", 5_000, "reply-driven")).toBe(
    EARLY_FINISH.latencySamples,
  );
  expect(confidenceSampleFloor("transfer", 4_000)).toBe(11);
  const expected = {
    short: { fast: 8, medium: 6, slow: 3 },
    long: { fast: 8, medium: 8, slow: 7 },
  };
  for (const preset of ["short", "long"] as const)
    for (const cadence of ["fast", "medium", "slow"] as const) {
      const durationMs = DURATION_PRESETS[preset].latencyMs;
      const floor = confidenceSampleFloor("latency", durationMs, cadence);
      expect(floor).toBe(expected[preset][cadence]);
      const intervalMs = fixedPingIntervalMs(cadence)!;
      const confidence = latencyConfidence(
        Array.from({ length: floor }, (_, i) => ({
          t: i * intervalMs,
          rtt: 20,
        })),
      );
      const armAt = Math.max(
        (floor - 1) * intervalMs,
        durationMs * EARLY_FINISH.minCoverage,
      );
      expect(
        shouldExitPhase({
          kind: "latency",
          cadence,
          elapsedMs: armAt,
          durationMs,
          confidence,
        }),
      ).toBe(true);
      expect(armAt + EARLY_FINISH.confirmationMs).toBeLessThan(durationMs);
    }
});

test("download sums consumed bytes and upload sums receiver means, never receiver durations", () => {
  const down = new ThroughputAggregate();
  down.begin("download", ["a", "b"], 0);
  down.observe(boundary(0, { a: 0, b: 0 }));
  down.addDownload("download", "a", 1_000);
  down.addDownload("download", "b", 3_000);
  down.observe(boundary(1_000, { a: 1_000, b: 3_000 }));
  expect(down.result("download", false).down).toMatchObject({
    reportedBytesPerSec: 4_000,
    totalBytes: 4_000,
  });

  const up = new ThroughputAggregate();
  up.begin("upload", ["a", "b"], 0);
  up.observe(
    boundary(
      0,
      {},
      { a: receiver("A", 100, 2e9), b: receiver("B", 500, 20e9, 13) },
    ),
  );
  up.observe(
    boundary(
      1_000,
      {},
      {
        a: receiver("A", 2_100, 4e9, 1_000),
        b: receiver("B", 9_500, 23e9, 1_019),
      },
    ),
  );
  expect(up.result("upload", false).up).toMatchObject({
    reportedBytesPerSec: 4_000,
    totalBytes: 11_000,
  });
  expect(up.intervals[0].full?.up?.map((c) => c.durationMs)).toEqual([
    2_000, 3_000,
  ]);
  expect(up.intervals[0].full?.up?.[1].endRequestMs).toBe(1_019);
  expect(up.serverResult("upload", "up", "b")).toMatchObject({
    reportedBytesPerSec: 3_000,
    totalBytes: 9_000,
  });
});

test("a window that moved nothing has no headline; one missing or unchanged receiver skips only its boundary", () => {
  const m = new ThroughputAggregate();
  m.begin("upload", ["a", "b"], 0);
  m.observe(
    boundary(0, {}, { a: receiver("a", 0, 1), b: receiver("b", 0, 1) }),
  );
  m.observe(
    boundary(
      1_000,
      {},
      { a: receiver("a", 0, 1e9 + 1), b: receiver("b", 0, 1e9 + 1) },
    ),
  );
  expect(m.result("upload", false).up).toBeNull();
  expect(
    m.observe(
      boundary(1_250, {}, { a: receiver("a", 0, 1_250e6 + 1), b: null }),
    ),
  ).toBeNull();
  expect(
    m.observe(
      boundary(
        1_300,
        {},
        { a: receiver("a", 9, 1_300e6 + 1), b: receiver("b", 0, 1e9 + 1) },
      ),
    ),
  ).toBeNull();
  const spanning = m.observe(
    boundary(
      1_500,
      {},
      {
        a: receiver("a", 1_000, 1_500e6 + 1),
        b: receiver("b", 500, 1_500e6 + 1),
      },
    ),
  );
  expect(spanning).toMatchObject({ startMs: 1_000, upBytesPerSec: 3_000 });
  expect(m.intervals).toHaveLength(1);
  // A restarted receiver is an evidence discontinuity, even in the same tick.
  m.observe(
    boundary(
      1_500,
      {},
      { a: receiver("restarted", 0, 1), b: receiver("b", 500, 1_500e6 + 1) },
    ),
  );
  expect(m.intervals.map((interval) => interval.reason)).toEqual([
    "stage-start",
    "evidence-resumed",
  ]);
  expect(m.result("upload", false).up).toBeNull();
});

test("every headline needs 800 ms in every clock and moved bytes", () => {
  const m = new ThroughputAggregate();
  m.begin("download", ["a", "b"], 0);
  m.observe(boundary(0, { a: 0, b: 0 }));
  m.observe(boundary(500, { a: 500, b: 2_000 }));
  expect(m.result("download", false).down).toBeNull();
  const idle = new ThroughputAggregate();
  idle.begin("download", ["a"], 0);
  idle.observe(boundary(0, { a: 0 }));
  idle.observe(boundary(1_000, { a: 0 }));
  expect(idle.result("download", false).down).toBeNull();
  m.observe(boundary(2_000, { a: 2_000, b: 8_000 }));
  expect(m.result("download", false).down?.reportedBytesPerSec).toBe(5_000);
  m.begin("download", ["a"], 2_000, "dropout");
  m.observe(boundary(2_000, { a: 2_000 }));
  m.observe(boundary(2_500, { a: 3_500 }));
  expect(m.result("download", false).down).toBeNull();
  expect(m.serverResult("download", "down", "b")?.reportedBytesPerSec).toBe(
    4_000,
  );
  m.observe(boundary(3_000, { a: 5_000 }));
  expect(m.result("download", false).down?.reportedBytesPerSec).toBe(3_000);

  const stable = new ThroughputAggregate();
  stable.begin("upload", ["a"], 0);
  stable.observe(boundary(0, {}, { a: receiver("a", 0, 1) }));
  stable.observe(boundary(2_000, {}, { a: receiver("a", 2_000, 2e9 + 1) }));
  stable.trackStable(1);
  // The stable window spans 900 ms of client time but only 500 ms of receiver time.
  stable.observe(boundary(2_900, {}, { a: receiver("a", 3_000, 2.5e9 + 1) }));
  expect(stable.result("upload", true).up).toMatchObject({
    reportedBytesPerSec: 1_200,
  });
});

test("opposite fluctuations use aggregate stability, simultaneous peaks and one stable boundary", () => {
  const m = new ThroughputAggregate();
  m.begin("upload", ["a", "b"], 0);
  let [a, b] = [0, 0];
  m.observe(
    boundary(0, {}, { a: receiver("a", a, 1), b: receiver("b", b, 1) }),
  );
  for (let i = 1; i <= 20; i++) {
    a += i % 2 ? 250 : 1_750;
    b += i % 2 ? 1_750 : 250;
    const nanos = 1 + i * 250e6;
    m.observe(
      boundary(
        i * 250,
        {},
        { a: receiver("a", a, nanos), b: receiver("b", b, nanos) },
      ),
    );
    m.trackStable(m.confidence().score);
  }
  expect(m.confidence().score).toBe(1);
  expect(m.result("upload", true).up).toMatchObject({
    reportedBytesPerSec: 8_000,
    peakBytesPerSec: 8_000,
  });
  const windows = m.intervals[0].headline!.up!;
  expect(windows[0].startNanos).toBe(windows[1].startNanos);
});

test("a replaced receiver id never spans a window; evidence resumes in a new interval", () => {
  const m = new ThroughputAggregate();
  m.begin("upload", ["a"], 0);
  m.observe(boundary(0, {}, { a: receiver("first", 0, 1) }));
  expect(
    m.observe(boundary(1_000, {}, { a: receiver("second", 500, 1e9) })),
  ).toBeNull();
  expect(m.intervals.map((interval) => interval.reason)).toEqual([
    "stage-start",
    "evidence-resumed",
  ]);
  expect(m.intervals[0].complete).toBe(false);
  expect(m.intervals[1].full).toBeNull();
});

test("idle confidence uses only in-window idle replies", () => {
  const server = new ServerLatency();
  for (let t = 0; t < 10; t++) server.observe("latency", reply(10), t * 100, 0);
  const idle = server.confidence();
  server.observe("download", reply(900), 1_000, 0);
  server.observe("latency", reply(900, false, false), 1_100, 0);
  expect(server.confidence()).toEqual(idle);
  expect(idle.sampleCount).toBe(10);
});

test("burst flushes cannot raise the peak above the fastest 500 ms", () => {
  const m = new ThroughputAggregate();
  m.begin("download", ["a"], 0);
  let bytes = 0;
  m.observe(boundary(0, { a: 0 }));
  // 50 ms flushes alternate between a 4x burst and silence: 2 000 B/s on average.
  for (let i = 1; i <= 40; i++) {
    bytes += i % 2 ? 200 : 0;
    m.observe(boundary(i * 50, { a: bytes }));
  }
  const down = m.result("download", false).down!;
  expect(down.reportedBytesPerSec).toBe(2_000);
  expect(down.peakBytesPerSec).toBeCloseTo(2_000, 6);
  expect(m.serverResult("download", "down", "a")!.peakBytesPerSec).toBe(2_000);
});

test("overlapping evidence never double counts bytes across intervals or stages", () => {
  const m = new ThroughputAggregate();
  m.begin("upload", ["a"], 0);
  m.observe(boundary(0, {}, { a: receiver("a", 100, 1) }));
  m.observe(boundary(1_000, {}, { a: receiver("a", 1_100, 1e9 + 1) }));
  m.begin("upload", ["a"], 1_000, "dropout");
  m.observe(boundary(1_000, {}, { a: receiver("a", 1_100, 1e9 + 1) }));
  m.observe(boundary(2_000, {}, { a: receiver("a", 2_100, 2e9 + 1) }));
  expect(m.totals("a").up).toBe(2_000);
  m.begin("bidirectional", ["a"], 2_200);
  m.observe(boundary(2_200, { a: 0 }, { a: receiver("b", 50_000, 1) }));
  m.observe(
    boundary(3_200, { a: 1_000 }, { a: receiver("b", 50_500, 1e9 + 1) }),
  );
  expect(m.result("bidirectional", false).up?.totalBytes).toBe(500);
  expect(m.totals("a").up).toBe(2_500);
});

type Vector = {
  name: string;
  stage: "download" | "upload" | "bidirectional";
  participants: string[];
  boundaries: {
    atMs: number;
    final?: boolean;
    dropout?: string[];
    down: Record<string, number>;
    up: Record<string, { id: string; bytes: number; nanos: number } | null>;
  }[];
  intervals: {
    reason: string;
    complete: boolean;
    window: {
      startMs: number;
      endMs: number;
      downBytesPerSec: number | null;
      upBytesPerSec: number | null;
    } | null;
  }[];
  peak: { downBytesPerSec: number | null; upBytesPerSec: number | null };
};
const vectors: Vector[] = await Bun.file(
  new URL("../../../../api/aggregation.testvectors.json", import.meta.url),
).json();

// The browser has not adopted the skipped stalled final boundary yet.
const adopted = vectors.filter((v) => !v.boundaries.some((b) => b.final));
for (const vector of adopted)
  test(`aggregation conformance: ${vector.name}`, () => {
    const m = new ThroughputAggregate();
    m.begin(vector.stage, vector.participants, vector.boundaries[0].atMs);
    let live = vector.participants;
    for (const b of vector.boundaries) {
      if (b.dropout) {
        live = live.filter((id) => !b.dropout!.includes(id));
        m.begin(vector.stage, live, b.atMs, "dropout");
      }
      m.observe({
        atMs: b.atMs,
        down: b.down,
        up: Object.fromEntries(
          Object.entries(b.up).map(([id, r]) => [
            id,
            r && receiver(r.id, r.bytes, r.nanos),
          ]),
        ),
      });
    }
    const intervals: Vector["intervals"] = m.intervals.map((interval) => ({
      reason: interval.reason,
      complete: interval.complete,
      window: interval.full && {
        startMs: interval.full.startMs,
        endMs: interval.full.endMs,
        downBytesPerSec: interval.full.downBytesPerSec,
        upBytesPerSec: interval.full.upBytesPerSec,
      },
    }));
    expect(intervals).toEqual(vector.intervals);
    expect({
      downBytesPerSec: m.peak(vector.stage, "down"),
      upBytesPerSec: m.peak(vector.stage, "up"),
    }).toEqual(vector.peak);
  });

const latencyVectors: {
  name: string;
  outcomes: { rttMs?: number; timeout?: boolean; break?: boolean }[];
  expect: Record<string, number | null>;
}[] = await Bun.file(
  new URL("../../../../api/latency.testvectors.json", import.meta.url),
).json();

for (const vector of latencyVectors)
  test(`latency conformance: ${vector.name}`, () => {
    const stats = new LatencyPopulation();
    let continuity = 0;
    for (const outcome of vector.outcomes)
      if (outcome.break) continuity++;
      else
        stats.observe(
          reply(outcome.rttMs ?? 250, !!outcome.timeout),
          continuity,
        );
    const s = stats.summary();
    const actual: Record<string, number | null> = {
      replies: (s?.probeCount ?? 0) - (s?.timeoutCount ?? 0),
      timeouts: s?.timeoutCount ?? 0,
      timeoutRatio: stats.timeoutRatio,
      p50Ms: s?.p50Ms ?? null,
      p95Ms: s?.p95Ms ?? null,
      jitterMs: s?.jitterMs ?? null,
      jitterPairs: s?.jitterPairs ?? 0,
    };
    expect(actual).toEqual(vector.expect);
  });
