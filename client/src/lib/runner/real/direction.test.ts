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

/** Drives performance.now and the timer queue together, so a budget measured in
 *  time can be asserted without waiting it out. */
function withClock<T>(body: (clock: Clock) => T): T {
  let now = 0;
  let nextId = 1;
  const timers = new Map<number, { at: number; fn: () => void }>();
  const realNow = performance.now.bind(performance);
  const realSet = globalThis.setTimeout;
  const realClear = globalThis.clearTimeout;
  performance.now = () => now;
  globalThis.setTimeout = ((fn: () => void, ms = 0) => {
    timers.set(nextId, { at: now + ms, fn });
    return nextId++;
  }) as unknown as typeof setTimeout;
  globalThis.clearTimeout = ((id: number) => {
    timers.delete(id);
  }) as unknown as typeof clearTimeout;

  const advance = (ms: number): void => {
    const target = now + ms;
    for (;;) {
      let due: [number, { at: number; fn: () => void }] | null = null;
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

function fakeHost(record: Recorded, clock: Clock): DirectionHost {
  const host = {
    failStage: () => record.skips.push(clock.now()),
    fail: (_reason: string, message: string) => record.fails.push(message),
    ingestThroughput: () => {},
    ingestLatency: () => {},
    emit: () => {},
    stall: () => {},
    resume: () => {},
  } as unknown as CoreHost;
  return {
    host: () => host,
    stallChanged: (detail) => record.stalls.push(detail ?? "stalled"),
    uploadProgress: () => {},
    beginUploadMeasure: () => {},
    discardTransfer: () => {},
  };
}

/** A lane that never carries a byte. `establishMs` is how long it takes to say
 *  so: 0 for a refused connection, the establish budget for a silent one. */
function deadLane(
  establishMs: number,
  events: LaneEvents,
  record: Recorded,
  clock: Clock,
): ByteLane {
  return {
    start(): void {
      record.starts.push(clock.now());
      setTimeout(() => events.onError(true, "no bytes"), establishMs);
    },
    measure(): void {},
    stop: () => Promise.resolve(),
    discard(): void {},
  };
}

// A recoverable establish failure stays with the runner-owned recovery budget.
// The direction may retry after its bounded backoff, but cannot independently
// skip the stage whether the failure is immediate or takes one establish bound.
for (const [what, establishMs] of [
  ["a lane refused at once", 0],
  ["a lane that goes silent", ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS],
] as const) {
  test(`${what} never independently skips its stage`, () => {
    withClock((clock) => {
      const record: Recorded = { skips: [], fails: [], starts: [], stalls: [] };
      const direction = new TransferDirection({
        dir: "down",
        stage: "download",
        laneCount: 1,
        warmupMs: 0,
        host: fakeHost(record, clock),
        lane: (_i, events) => deadLane(establishMs, events, record, clock),
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

// The deadline covers a path that never worked. One that did is a drop, and a
// drop is restarted until the restart bound, whatever the stage has left to run.
test("a lane that carried bytes is restarted past the skip deadline", () => {
  withClock((clock) => {
    const record: Recorded = { skips: [], fails: [], starts: [], stalls: [] };
    let carried = false;
    const direction = new TransferDirection({
      dir: "up",
      stage: "upload",
      laneCount: 1,
      warmupMs: 0,
      host: fakeHost(record, clock),
      lane: (_i, events) => ({
        start(): void {
          record.starts.push(clock.now());
          if (!carried) {
            carried = true;
            events.onAlive();
          }
          setTimeout(() => events.onError(true, "dropped"), 10);
        },
        measure(): void {},
        stop: () => Promise.resolve(),
        discard(): void {},
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
    const host = {
      failStage() {},
      fail() {},
      ingestThroughput() {
        ingests++;
      },
      ingestLatency() {},
      emit() {},
      stall() {},
      resume() {},
    } as unknown as CoreHost;
    const direction = new TransferDirection({
      dir: "up",
      stage: "upload",
      laneCount: 1,
      warmupMs: 0,
      host: {
        host: () => host,
        stallChanged: () => {},
        uploadProgress: () => {},
        uploadPresentationHint: (lane, bytes, elapsedMs, generation) =>
          hints.push([lane, bytes, elapsedMs, generation]),
        beginUploadMeasure: () => {},
        discardTransfer: () => {},
      },
      lane: (_i, events) => ({
        start: () => events.onAlive(640, 80),
        measure: () => {},
        stop: () => Promise.resolve(),
        discard: () => {},
      }),
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
    const record: Recorded = { skips: [], fails: [], starts: [], stalls: [] };
    const direction = new TransferDirection({
      dir: "up",
      stage: "bidirectional",
      laneCount: 1,
      warmupMs: 0,
      host: fakeHost(record, clock),
      lane: (_i, events) => ({
        start: () => events.onError(false, "HTTP 429"),
        measure: () => {},
        stop: () => Promise.resolve(),
        discard: () => {},
      }),
    });
    direction.spawn(["https://meter.test/lane"]);

    expect(record.skips).toEqual([0]);
    expect(record.fails).toEqual([]);
    expect(record.stalls).toEqual([]);
  });
});

// performance.now is coarsened per origin, so two reports arriving while a lane
// stops can land in the same tick. The second window has no duration to divide
// by, and its bytes have to survive into the next one that does.
test("bytes reported inside one clock tick reach the next aggregate", () => {
  withClock((clock) => {
    const bytes: number[] = [];
    const host = {
      failStage() {},
      fail() {},
      ingestThroughput(_dir: string, _rate: number, delta: number) {
        bytes.push(delta);
      },
      ingestLatency() {},
      emit() {},
      stall() {},
      resume() {},
    } as unknown as CoreHost;
    const direction = new TransferDirection({
      dir: "down",
      stage: "download",
      laneCount: 1,
      warmupMs: 0,
      host: {
        host: () => host,
        stallChanged: () => {},
        uploadProgress: () => {},
        beginUploadMeasure: () => {},
        discardTransfer: () => {},
      },
      lane: (_i, events) => ({
        start() {},
        measure() {},
        stop() {
          // Both reports aggregate immediately (the direction is stopping), the
          // first one in the same tick as the stop's own flush.
          events.onProgress(17, 25, 1);
          clock.advance(1);
          events.onProgress(5, 25, 1);
          return Promise.resolve();
        },
        discard() {},
      }),
    });

    direction.spawn(["https://meter.test/lane"]);
    direction.measure();
    void direction.stop();

    expect(bytes.reduce((sum, delta) => sum + delta, 0)).toBe(22);
  });
});

test("graceful stop aggregates a lane's final progress report", async () => {
  const bytes: number[] = [];
  const host = {
    failStage() {},
    fail() {},
    ingestThroughput(_dir: string, _rate: number, delta: number) {
      bytes.push(delta);
    },
    ingestLatency() {},
    emit() {},
    stall() {},
    resume() {},
  } as unknown as CoreHost;
  const deps: DirectionHost = {
    host: () => host,
    stallChanged: () => {},
    uploadProgress: () => {},
    beginUploadMeasure: () => {},
    discardTransfer: () => {},
  };
  const direction = new TransferDirection({
    dir: "down",
    stage: "download",
    laneCount: 1,
    warmupMs: 0,
    host: deps,
    lane: (_i, events) => ({
      start() {},
      measure() {},
      async stop() {
        await new Promise((resolve) => setTimeout(resolve, 1));
        events.onProgress(17, 25, 1);
      },
      discard() {},
    }),
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
    const record: Recorded = { skips: [], fails: [], starts: [], stalls: [] };
    const states: boolean[] = [];
    let direction!: TransferDirection;
    const host = fakeHost(record, clock);
    host.stallChanged = () => states.push(direction.stalled);
    direction = new TransferDirection({
      dir: "up",
      stage: "bidirectional",
      laneCount: 1,
      warmupMs: 0,
      host,
      lane: (_i, _events) => ({
        start() {},
        measure() {},
        stop: () => Promise.resolve(),
        discard() {},
      }),
    });

    direction.spawn(["https://meter.test/wt/upload"]);
    direction.measure();
    clock.advance(DIRECTION_PROGRESS_WINDOW_MS + 1);

    expect(states).toEqual([true]);
    direction.noteMeasuredProgress(0);
    expect(states).toEqual([true]);

    // A slow direction recovers on its own first receiver-counted byte and
    // stays healthy while bytes remain inside the window.
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
    const record: Recorded = { skips: [], fails: [], starts: [], stalls: [] };
    const states: boolean[] = [];
    let direction!: TransferDirection;
    const host = fakeHost(record, clock);
    host.stallChanged = () => states.push(direction.stalled);
    direction = new TransferDirection({
      dir: "up",
      stage: "upload",
      laneCount: 1,
      warmupMs: 0,
      host,
      lane: () => ({
        start() {},
        measure() {},
        stop: () => Promise.resolve(),
        discard() {},
      }),
    });
    direction.spawn(["https://meter.test/wt/upload"]);
    direction.measure();
    direction.discard();
    clock.advance(DIRECTION_PROGRESS_WINDOW_MS * 2);

    expect(states).toEqual([]);
  });
});
