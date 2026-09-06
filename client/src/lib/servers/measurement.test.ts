import { expect, test } from "bun:test";
import {
  AggregateMeasurements,
  weakestLatencyConfidence,
  type Boundary,
} from "./measurement";
import { DEFAULT_CONFIG } from "../state/defaults";

test("a quieter stable server cannot hide another server's unsettled latency or reorder completion confidence", () => {
  const unsettled = {
    confidence: () => ({
      score: 0.1,
      sampleCount: 100,
      jitterRatio: 0.8,
      lossRatio: 0.2,
    }),
  };
  const sparse = {
    confidence: () => ({
      score: 1,
      sampleCount: 20,
      jitterRatio: 0,
      lossRatio: 0,
    }),
  };
  for (const population of [
    [unsettled, sparse],
    [sparse, unsettled],
  ])
    expect(weakestLatencyConfidence(population)).toEqual({
      score: 0.1,
      sampleCount: 20,
      jitterRatio: 0.8,
      lossRatio: 0.2,
    });
});

function receiver(id: string, bytes: number, nanos: number, request = 0) {
  return {
    id,
    bytes,
    nanos,
    requestedAtMs: request,
    receivedAtMs: request + 7,
  };
}
function boundary(
  atMs: number,
  down: Record<string, number> = {},
  up: Boundary["up"] = {},
): Boundary {
  return { atMs, down, up };
}

test("download sums consumed bytes over one client window", () => {
  const m = new AggregateMeasurements();
  m.begin("download", ["a", "b"], 0);
  m.observe(boundary(0, { a: 0, b: 0 }));
  m.addDownload("a", 1000);
  m.addDownload("b", 3000);
  m.observe(boundary(1000, { a: 1000, b: 3000 }));
  m.close();
  expect(m.result("download", "down", false)?.reportedBytesPerSec).toBe(4000);
  expect(m.result("download", "down", false)?.totalBytes).toBe(4000);
  expect(m.intervals[0].full?.down?.map((window) => window.durationMs)).toEqual(
    [1000, 1000],
  );
});
test("upload sums component receiver means, never receiver durations", () => {
  const m = new AggregateMeasurements();
  m.begin("upload", ["a", "b"], 0);
  m.observe(
    boundary(
      0,
      {},
      { a: receiver("A", 100, 2e9, 0), b: receiver("B", 500, 20e9, 13) },
    ),
  );
  m.observe(
    boundary(
      1000,
      {},
      { a: receiver("A", 2100, 4e9, 1000), b: receiver("B", 9500, 23e9, 1019) },
    ),
  );
  m.close();
  const result = m.result("upload", "up", false);
  expect(result?.reportedBytesPerSec).toBe(4000);
  expect(result?.totalBytes).toBe(11000);
  expect(m.intervals[0].full?.up?.map((window) => window.durationMs)).toEqual([
    2000, 3000,
  ]);
  expect(m.intervals[0].full?.up?.[1].endRequestMs).toBe(1019);
});
test("opposite fluctuations use aggregate stability and simultaneous peaks", () => {
  const m = new AggregateMeasurements();
  m.begin("upload", ["a", "b"], 0);
  let a = 0,
    b = 0;
  m.observe(
    boundary(0, {}, { a: receiver("a", a, 1), b: receiver("b", b, 1) }),
  );
  for (let i = 1; i <= 20; i++) {
    a += i % 2 ? 250 : 1750;
    b += i % 2 ? 1750 : 250;
    m.observe(
      boundary(
        i * 250,
        {},
        {
          a: receiver("a", a, 1 + i * 250e6),
          b: receiver("b", b, 1 + i * 250e6),
        },
      ),
    );
    m.trackStable(m.confidence().score, DEFAULT_CONFIG.adaptive);
  }
  const result = m.result("upload", "up", true)!;
  expect(m.confidence().score).toBe(1);
  expect(result.reportedBytesPerSec).toBe(8000);
  expect(result.peakBytesPerSec).toBe(8000);
  const windows = m.intervals[0].headline!.up!;
  expect(windows[0].startNanos).toBe(windows[1].startNanos);
});
test("measured zero is valid, missing receiver evidence is unavailable", () => {
  const m = new AggregateMeasurements();
  m.begin("upload", ["a", "b"], 0);
  m.observe(
    boundary(0, {}, { a: receiver("a", 0, 1), b: receiver("b", 0, 1) }),
  );
  m.observe(
    boundary(
      1000,
      {},
      { a: receiver("a", 0, 1e9 + 1), b: receiver("b", 0, 1e9 + 1) },
    ),
  );
  expect(m.result("upload", "up", false)?.reportedBytesPerSec).toBe(0);
  m.observe(boundary(1250, {}, { a: receiver("a", 0, 1250e6 + 1), b: null }));
  expect(m.result("upload", "up", false)).toBeNull();
  expect(m.intervals[0].full?.upBytesPerSec).toBe(0);
  m.observe(
    boundary(
      1500,
      {},
      { a: receiver("a", 0, 1500e6 + 1), b: receiver("b", 0, 1500e6 + 1) },
    ),
  );
  expect(m.confidence().sampleCount).toBe(0);
  expect(m.intervals).toHaveLength(2);
  expect(m.result("upload", "up", false)).toBeNull();
});
test("dropout revokes stable evidence and final headline needs a survivor interval", () => {
  const m = new AggregateMeasurements();
  m.begin("download", ["a", "b"], 0);
  m.observe(boundary(0, { a: 0, b: 0 }));
  m.observe(boundary(2000, { a: 2000, b: 8000 }));
  expect(m.result("download", "down", false)?.reportedBytesPerSec).toBe(5000);
  m.begin("download", ["a"], 2000, "dropout");
  m.observe(boundary(2000, { a: 2000 }));
  m.observe(boundary(2500, { a: 3500 }));
  expect(m.result("download", "down", false)).toBeNull();
  expect(m.intervals[0].full?.downBytesPerSec).toBe(5000);
  m.observe(boundary(3000, { a: 5000 }));
  expect(m.result("download", "down", false)?.reportedBytesPerSec).toBe(3000);
  expect(m.intervals[1].full?.down?.map((window) => window.serverId)).toEqual([
    "a",
  ]);
  m.begin("download", [], 3000, "dropout");
  m.observe(boundary(3250, { a: 10000, b: 100000 }));
  expect(m.result("download", "down", false)).toBeNull();
});
test("progress and overlapping checkpoints cannot double count bytes across intervals", () => {
  const m = new AggregateMeasurements();
  m.begin("upload", ["a"], 0);
  m.observe(boundary(0, {}, { a: receiver("a", 100, 1) }));
  m.observeUpload("a", { id: "a", bytes: 1500 });
  m.observe(boundary(1000, {}, { a: receiver("a", 1100, 1e9 + 1) }));
  m.begin("upload", ["a"], 1000, "dropout");
  m.observe(boundary(1000, {}, { a: receiver("a", 1100, 1e9 + 1) }));
  m.observeUpload("a", { id: "a", bytes: 2100 });
  m.observe(boundary(2000, {}, { a: receiver("a", 2100, 2e9 + 1) }));
  expect(m.totals("a").up).toBe(2000);
  m.begin("bidirectional", ["a"], 2200);
  m.observe(boundary(2200, { a: 0 }, { a: receiver("b", 50000, 1) }));
  m.observe(boundary(3200, { a: 1000 }, { a: receiver("b", 50500, 1e9 + 1) }));
  expect(m.stageTotals("upload", "a").up).toBe(2000);
  expect(m.result("upload", "up", false)?.totalBytes).toBe(2000);
  expect(m.result("bidirectional", "up", false)?.totalBytes).toBe(500);
});
