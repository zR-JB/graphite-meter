import { test, expect } from "bun:test";
import {
  TransferDirection,
  transferStageStalled,
  type DirectionHost,
} from "./direction";
import {
  DIRECTION_PROGRESS_WINDOW_MS,
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
  LANE_RESTART_BACKOFF_MS,
} from "./budgets";
import type { ByteLane, LaneEvents } from "./byteLane";
import type { CoreHost } from "../core";

interface Clock {
  now(): number;
  advance(ms: number): void;
}
function withClock<T>(body: (clock: Clock) => T): T {
  let now = 0,
    nextId = 1;
  const timers = new Map<number, { at: number; fn: () => void }>();
  const realNow = performance.now.bind(performance),
    realSet = globalThis.setTimeout,
    realClear = globalThis.clearTimeout;
  performance.now = () => now;
  globalThis.setTimeout = ((fn: () => void, ms = 0) => {
    timers.set(nextId, { at: now + ms, fn });
    return nextId++;
  }) as unknown as typeof setTimeout;
  globalThis.clearTimeout = ((id: number) =>
    timers.delete(id)) as unknown as typeof clearTimeout;
  const advance = (ms: number): void => {
    const target = now + ms;
    for (;;) {
      let due: [number, { at: number; fn: () => void }] | undefined;
      for (const entry of timers)
        if (entry[1].at <= target && (!due || entry[1].at < due[1].at))
          due = entry;
      if (!due) break;
      timers.delete(due[0]);
      now = due[1].at;
      due[1].fn();
    }
    now = target;
  };
  try {
    return body({ now: () => now, advance });
  } finally {
    performance.now = realNow;
    globalThis.setTimeout = realSet;
    globalThis.clearTimeout = realClear;
  }
}
interface Recorded {
  skips: number[];
  fails: string[];
  starts: number[];
  stalls: string[];
}
const recording = (): Recorded => ({
  skips: [],
  fails: [],
  starts: [],
  stalls: [],
});
function fakeHost(
  record: Recorded,
  clock: Clock,
  core: Record<string, unknown> = {},
): DirectionHost {
  const host = {
    failStage: () => record.skips.push(clock.now()),
    fail: (_reason: string, message: string) => record.fails.push(message),
    ingestThroughput: () => {},
    ingestLatency: () => {},
    emit: () => {},
    stall: () => {},
    resume: () => {},
    ...core,
  } as unknown as CoreHost;
  return {
    host: () => host,
    stallChanged: (detail) => record.stalls.push(detail ?? "stalled"),
    uploadProgress: () => {},
    beginUploadMeasure: () => {},
    discardTransfer: () => {},
  };
}
type StartLane = () => void;
type StopLane = () => void | Promise<void>;
function lane(
  start: StartLane = () => {},
  stop: StopLane = () => {},
): ByteLane {
  return {
    start,
    measure(): void {},
    stop: () => Promise.resolve(stop()),
    discard(): void {},
  };
}
function newDirection(
  clock: Clock,
  record: Recorded = recording(),
  options: {
    dir?: "up" | "down";
    stage?: "upload" | "download" | "bidirectional";
    host?: DirectionHost;
    makeLane?: (events: LaneEvents) => ByteLane;
  } = {},
): TransferDirection {
  return new TransferDirection({
    dir: options.dir ?? "up",
    stage: options.stage ?? "upload",
    laneCount: 1,
    warmupMs: 0,
    host: options.host ?? fakeHost(record, clock),
    lane: (_i, events) => options.makeLane?.(events) ?? lane(),
  });
}
for (const [what, establishMs] of [
  ["a lane refused at once", 0],
  ["a lane that goes silent", ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS],
] as const) {
  test(`${what} never independently skips its stage`, () => {
    withClock((clock) => {
      const record = recording();
      const direction = newDirection(clock, record, {
        dir: "down",
        stage: "download",
        makeLane: (events) =>
          lane(() => {
            record.starts.push(clock.now());
            setTimeout(() => events.onError(true, "no bytes"), establishMs);
          }),
      });
      direction.spawn(["https://meter.test/lane"]);
      direction.measure();
      clock.advance(
        (ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS + LANE_RESTART_BACKOFF_MS) *
          4,
      );
      expect(record.skips).toEqual([]);
      expect(record.fails).toEqual([]);
      expect(record.stalls).toHaveLength(1);
      expect(record.starts.length).toBeGreaterThan(1);
    });
  });
}
test("a lane that carried bytes is restarted past the skip deadline", () => {
  withClock((clock) => {
    const record = recording();
    let carried = false;
    const direction = newDirection(clock, record, {
      makeLane: (events) =>
        lane(() => {
          record.starts.push(clock.now());
          if (!carried) {
            carried = true;
            events.onAlive();
          }
          setTimeout(() => events.onError(true, "dropped"), 10);
        }),
    });
    direction.spawn(["https://meter.test/lane"]);
    clock.advance(20_000);
    expect(record.skips).toEqual([]);
    expect(record.starts.length).toBeGreaterThan(1);
    clock.advance(5_000);
    expect(record.skips).toEqual([]);
    expect(record.fails).toEqual([]);
  });
});
test("a local upload completion is a presentation hint, not measurement evidence", () => {
  withClock((clock) => {
    const hints: [number, number, number, number][] = [];
    let ingests = 0;
    const deps = fakeHost(recording(), clock, {
      ingestThroughput() {
        ingests++;
      },
    });
    deps.uploadPresentationHint = (lane, bytes, elapsedMs, generation) =>
      hints.push([lane, bytes, elapsedMs, generation]);
    const direction = newDirection(clock, undefined, {
      host: deps,
      makeLane: (events) => lane(() => events.onAlive(640, 80)),
    });
    direction.setUploadGeneration(7);
    direction.spawn(["https://meter.test/upload"]);
    expect(hints).toEqual([[0, 640, 80, 7]]);
    expect(ingests).toBe(0);
    clock.advance(DIRECTION_PROGRESS_WINDOW_MS + 1);
    expect(ingests).toBe(0);
  });
});
test("a permanent lane refusal finalizes only its affected stage", () => {
  withClock((clock) => {
    const record = recording();
    const direction = newDirection(clock, record, {
      stage: "bidirectional",
      makeLane: (events) => lane(() => events.onError(false, "HTTP 429")),
    });
    direction.spawn(["https://meter.test/lane"]);
    expect(record.skips).toEqual([0]);
    expect(record.fails).toEqual([]);
    expect(record.stalls).toEqual([]);
  });
});
test("an explicit invalid upload id enters runner recovery without a same-id restart", () => {
  withClock((clock) => {
    const record = recording();
    let refuse = (): void => {};
    const direction = newDirection(clock, record, {
      makeLane: (events) =>
        lane(() => {
          record.starts.push(clock.now());
          refuse = () => events.onError(true, "HTTP 400", "unknown-upload-id");
        }),
    });
    direction.spawn(["https://meter.test/lane"]);
    direction.measure();
    refuse();
    clock.advance(LANE_RESTART_BACKOFF_MS * 2);
    expect(record.starts).toEqual([0]);
    expect(record.skips).toEqual([]);
    expect(record.stalls).toEqual(["HTTP 400"]);
  });
});
test("bytes reported inside one clock tick reach the next aggregate", () => {
  withClock((clock) => {
    const bytes: number[] = [];
    const deps = fakeHost(recording(), clock, {
      ingestThroughput(_dir: string, _rate: number, delta: number) {
        bytes.push(delta);
      },
    });
    const direction = newDirection(clock, undefined, {
      dir: "down",
      stage: "download",
      host: deps,
      makeLane: (events) =>
        lane(
          () => {},
          () => {
            events.onProgress(17, 25, 1);
            clock.advance(1);
            events.onProgress(5, 25, 1);
          },
        ),
    });
    direction.spawn(["https://meter.test/lane"]);
    direction.measure();
    void direction.stop();
    expect(bytes.reduce((sum, delta) => sum + delta, 0)).toBe(22);
  });
});
test("graceful stop aggregates a lane's final progress report", async () => {
  const bytes: number[] = [];
  const deps = fakeHost(
    recording(),
    { now: () => performance.now(), advance: (ms) => setTimeout(() => {}, ms) },
    {
      ingestThroughput(_dir: string, _rate: number, delta: number) {
        bytes.push(delta);
      },
    },
  );
  const direction = new TransferDirection({
    dir: "down",
    stage: "download",
    laneCount: 1,
    warmupMs: 0,
    host: deps,
    lane: (_i, events) =>
      lane(
        () => {},
        async () => {
          await new Promise((resolve) => setTimeout(resolve, 1));
          events.onProgress(17, 25, 1);
        },
      ),
  });
  direction.spawn(["https://meter.test/wt/download"]);
  direction.measure();
  await direction.stop();
  expect(bytes.reduce((sum, delta) => sum + delta, 0)).toBe(17);
});
test("a healthy download cannot mask a stalled upload", () => {
  expect(transferStageStalled([{ stalled: false }, { stalled: true }])).toBe(
    true,
  );
  expect(transferStageStalled([{ stalled: false }, { stalled: false }])).toBe(
    false,
  );
});
test("a silently pending measured direction stalls independently", () => {
  withClock((clock) => {
    const record = recording(),
      states: boolean[] = [];
    let direction!: TransferDirection;
    const host = fakeHost(record, clock);
    host.stallChanged = () => states.push(direction.stalled);
    direction = newDirection(clock, record, {
      stage: "bidirectional",
      host,
      makeLane: () => lane(),
    });
    direction.spawn(["https://meter.test/wt/upload"]);
    direction.measure();
    clock.advance(DIRECTION_PROGRESS_WINDOW_MS + 1);
    expect(states).toEqual([true]);
    direction.noteMeasuredProgress(0);
    expect(states).toEqual([true]);
    direction.noteMeasuredProgress(1);
    expect(states).toEqual([true, false]);
    clock.advance(DIRECTION_PROGRESS_WINDOW_MS - 1);
    expect(states).toEqual([true, false]);
    clock.advance(2);
    expect(states).toEqual([true, false, true]);
    direction.noteMeasuredProgress(1);
    expect(states).toEqual([true, false, true, false]);
    void direction.stop();
    clock.advance(DIRECTION_PROGRESS_WINDOW_MS * 2);
    expect(states).toEqual([true, false, true, false]);
  });
});
test("discard cancels a pending direction watchdog", () => {
  withClock((clock) => {
    const record = recording(),
      states: boolean[] = [];
    let direction!: TransferDirection;
    const host = fakeHost(record, clock);
    host.stallChanged = () => states.push(direction.stalled);
    direction = newDirection(clock, record, { host, makeLane: () => lane() });
    direction.spawn(["https://meter.test/wt/upload"]);
    direction.measure();
    direction.discard();
    clock.advance(DIRECTION_PROGRESS_WINDOW_MS * 2);
    expect(states).toEqual([]);
  });
});
