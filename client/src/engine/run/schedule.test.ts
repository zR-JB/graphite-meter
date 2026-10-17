import { expect, test } from "bun:test";
import { replan, timeline, truncate, type Plan } from "./schedule";

const plan = (stages: Partial<Plan["stages"]>, duration: Partial<Plan["duration"]> = {}): Plan => ({
  stages: { latency: false, download: false, upload: false, bidirectional: false, ...stages },
  duration: {
    warmupMs: 500,
    latencyMs: 2_000,
    downloadMs: 4_000,
    uploadMs: 4_000,
    bidirectionalMs: 4_000,
    ...duration,
  },
  skipLoadedLatencyWhenStageOff: false,
});
const spans = (segments: ReturnType<typeof timeline>) => segments.map(({ phase, start, end }) => [phase, start, end]);

test("a live change keeps what ran, gives the running segment its new length but never less than now, then follows", () => {
  const before = plan({ download: true, upload: true });
  const running = timeline(before);
  // Mid-download, the download is cut to 1 s, which has already passed, and bidirectional is added.
  const after = plan({ download: true, upload: true, bidirectional: true }, { downloadMs: 1_000 });
  expect(spans(replan(running, 2_000, after))).toEqual([
    ["warmup", 0, 500],
    ["download", 500, 2_000],
    ["warmup", 2_000, 2_500],
    ["upload", 2_500, 6_500],
    ["warmup", 6_500, 7_000],
    ["bidirectional", 7_000, 11_000],
  ]);
  // A warmup under way runs on, but a stage turned off loses its window. A stage kept follows its warmup's new length.
  const warming = replan(running, 4_700, plan({ download: true }, { downloadMs: 1_000 }));
  expect(spans(warming)).toEqual([
    ["warmup", 0, 500],
    ["download", 500, 4_500],
    ["warmup", 4_500, 5_000],
  ]);
  expect(spans(replan(running, 4_700, plan({ download: true, upload: true }, { warmupMs: 100 })))).toEqual([
    ["warmup", 0, 500],
    ["download", 500, 4_500],
    ["warmup", 4_500, 4_700],
    ["upload", 4_700, 8_700],
  ]);
  // Nothing runs between segments, so only what has not begun is replanned.
  expect(spans(replan(running, 4_500, plan({ download: true }), true))).toEqual([
    ["warmup", 0, 500],
    ["download", 500, 4_500],
  ]);
});

test("an early finish ends the running segment now and moves what follows forward", () => {
  const segments = timeline(plan({ latency: true, download: true }));
  const latency = segments[1]!;
  expect(spans(truncate(segments, latency, 1_500))).toEqual([
    ["warmup", 0, 500],
    ["latency", 500, 1_500],
    ["warmup", 1_500, 2_000],
    ["download", 2_000, 6_000],
  ]);
});
