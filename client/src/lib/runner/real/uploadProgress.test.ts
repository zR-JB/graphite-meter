// Corners of the upload meter that the RealRunner and wtStage integration pins
// do not reach: a superseded external attach, a discard arriving while a
// finalizing teardown still holds the worker, and the server-count clamp that
// keeps two feeds for one upload id from moving the curve backwards.
import { test, expect } from "bun:test";
import type { CoreHost } from "../core";
import type { FetchThroughputTarget } from "../../api/endpoints";
import {
  UploadProgressChannel,
  type UploadProgressLane,
} from "./uploadProgress";

class FakeWorker {
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  readonly sent: unknown[] = [];
  terminated = 0;
  static last: FakeWorker | null = null;

  constructor() {
    FakeWorker.last = this;
  }
  postMessage(message: unknown): void {
    this.sent.push(message);
  }
  terminate(): void {
    this.terminated++;
  }
}

const target: FetchThroughputTarget = {
  id: "http://meter.test:7246",
  origin: "http://meter.test:7246",
  transport: "fetch-stream",
  protocol: "http1",
  tls: false,
  routes: {
    probe: "/probe",
    download: "/download",
    upload: "/upload",
    uploadSession: "/upload/session",
    uploadProgress: "/upload/progress",
  },
};

function channelUnderTest(
  laneState: Partial<UploadProgressLane> = {},
  sampleProvesStageLiveness = true,
): {
  channel: UploadProgressChannel;
  failures: string[];
  curve: number[];
  liveness: boolean[];
  progress: number[];
  stalls: { detail?: string; cause?: string }[];
  recoveryGaps: number[];
} {
  const failures: string[] = [];
  /** Byte delta of every frame the channel fed into the live curve. */
  const curve: number[] = [];
  const liveness: boolean[] = [];
  const progress: number[] = [];
  const stalls: { detail?: string; cause?: string }[] = [];
  const recoveryGaps: number[] = [];
  const lane: UploadProgressLane = {
    stage: "upload",
    measuring: false,
    stageSawBytes: false,
    ...laneState,
  };
  const host = {
    failStage(_stage: string, _reason: string, message: string) {
      failures.push(message);
    },
    fail(_reason: string, message: string) {
      failures.push(message);
    },
    ingestThroughput(
      _dir: string,
      _rate: number,
      bytesDelta: number,
      _duration: number,
      _authoritative: boolean,
      provesLiveness = true,
    ) {
      curve.push(bytesDelta);
      liveness.push(provesLiveness);
    },
    recordRecoveryGap(_dir: string, seconds: number) {
      recoveryGaps.push(seconds);
    },
  } as unknown as CoreHost;
  return {
    channel: new UploadProgressChannel({
      host: () => host,
      sampleProvesStageLiveness: () => sampleProvesStageLiveness,
      target: () => target,
      lane: () => lane,
      transferActive: () => true,
      discardTransfer: () => {},
      noteLaneProgress: (bytes) => progress.push(bytes),
      setLaneStalled: (_stalled, detail, cause) =>
        stalls.push({ detail, cause }),
    }),
    failures,
    curve,
    liveness,
    progress,
    stalls,
    recoveryGaps,
  };
}

// Only one owner may act on an attach outcome, so a replaced or torn-down feed
// resolves "superseded" rather than failing the stage on its establish timeout.
test("attachExternal: a replaced feed is superseded, not a stage failure", async () => {
  const { channel, failures } = channelUnderTest();
  const first = channel.attachExternal(() => {});
  const second = channel.attachExternal(() => {});
  expect(await first).toBe("superseded");

  await channel.teardown(false);
  expect(await second).toBe("superseded");
  expect(failures).toEqual([]);
});

test("an old upload generation cannot feed the replacement meter", async () => {
  const { channel, curve } = channelUnderTest({ measuring: true });
  const first = channel.attachExternal(() => {});
  const oldGeneration = channel.generation;
  const second = channel.attachExternal(() => {});

  channel.accept({ type: "bytes", n: 9_999, t: 1_000_000_000 }, oldGeneration);
  expect(curve).toEqual([]);

  channel.accept({ type: "bytes", n: 100, t: 1_000_000_000 });
  channel.accept({ type: "bytes", n: 250, t: 2_000_000_000 });
  expect(curve).toEqual([150]);
  await channel.teardown(false);
  expect(await first).toBe("superseded");
  expect(await second).toBe("superseded");
});

// A refused feed ends the attach as surely as a ready record: left pending, the
// runner would receive both a recovery request and an establish timeout.
test("accept: a refusal ends a pending external attach", async () => {
  const { channel, failures } = channelUnderTest({ measuring: true });
  const attached = channel.attachExternal(() => {});

  channel.accept({
    type: "fatal",
    detail: "session closed",
    cause: "transient-connection",
  });
  // Racing an already-settled sentinel reports an attach left pending as a
  // value rather than as a whole-test timeout.
  const outcome = await Promise.race([attached, Promise.resolve("pending")]);
  expect(outcome).toBe("superseded");
  expect(failures).toEqual([]);
});

test("an explicit invalid upload id starts runner-owned recovery", () => {
  const { channel, failures, stalls } = channelUnderTest({ measuring: true });

  channel.accept({
    type: "fatal",
    detail: "unknown upload id",
    cause: "unknown-upload-id",
  });

  expect(failures).toEqual([]);
  expect(stalls).toEqual([
    { detail: "unknown upload id", cause: "unknown-upload-id" },
  ]);
});

test("capacity and ownership refusals cannot trigger upload-id recovery", () => {
  for (const cause of ["capacity-refusal", "owner-mismatch"] as const) {
    const { channel, failures, stalls } = channelUnderTest({ measuring: true });
    channel.accept({ type: "fatal", detail: cause, cause });
    expect(failures).toEqual([cause]);
    expect(stalls).toEqual([]);
  }
});

// The session worker owns the finalizing DELETE and sends it when the terminal
// record lands. A second one from here is a DELETE against an upload id the next
// stage may already have taken.
test("teardown finalizes a dropped session feed, but not a completed one", async () => {
  const dropped = channelUnderTest({ measuring: true });
  let droppedFinalizes = 0;
  void dropped.channel.attachExternal(() => droppedFinalizes++);
  await dropped.channel.teardown(true);
  expect(droppedFinalizes).toBe(1);

  const { channel } = channelUnderTest({ measuring: true });
  let finalizes = 0;
  const attached = channel.attachExternal(() => finalizes++);
  channel.accept({ type: "open" });
  expect(await attached).toBe("open");

  channel.accept({ type: "complete", n: 4096, t: 1_000_000_000 });
  await channel.teardown(true);
  expect(finalizes).toBe(0);
});

// One upload id can be reported by two feeds at once while a session feed
// replaces an HTTP one. The server aggregate is cumulative, so the replacement's
// first frames arrive behind the count already shown; taking them would feed the
// curve a negative delta and then double-count the catch-up.
test("a server count that arrives behind the last one does not move the curve", () => {
  const { channel, curve } = channelUnderTest({ measuring: true });

  channel.accept({ type: "bytes", n: 1000, t: 1_000_000_000 });
  channel.accept({ type: "bytes", n: 2000, t: 2_000_000_000 });
  channel.accept({ type: "bytes", n: 500, t: 3_000_000_000 });
  channel.accept({ type: "bytes", n: 2600, t: 4_000_000_000 });
  expect(curve).toEqual([1000, 0, 600]);
});

test("upload recovery bytes stay accounted without resuming a stalled sibling", () => {
  const { channel, curve, liveness, progress } = channelUnderTest(
    { measuring: true },
    false,
  );

  channel.accept({ type: "bytes", n: 100, t: 1_000_000_000 });
  channel.accept({ type: "bytes", n: 250, t: 2_000_000_000 });

  expect(curve).toEqual([150]);
  expect(liveness).toEqual([false]);
  expect(progress).toEqual([150]);
});

test("a rotation gap is reduced once without a chart sample", () => {
  const { channel, curve, recoveryGaps } = channelUnderTest({
    measuring: true,
  });
  channel.beginRecoveryGap();
  channel.accept({ type: "bytes", n: 100, t: 1_000_000_000 });
  channel.accept({ type: "bytes", n: 250, t: 2_000_000_000 });
  channel.accept({ type: "bytes", n: 400, t: 3_000_000_000 });

  expect(recoveryGaps).toHaveLength(1);
  expect(recoveryGaps[0]).toBeGreaterThanOrEqual(0);
  expect(curve).toEqual([150, 150]);
});

// Unreachable today (every path tears down first), but a worker left running
// under a new feed would keep pushing its own upload id's cumulative count into
// the next stage's meter, which the monotonic guard accepts.
test("taking a session feed terminates the worker feed it replaces", async () => {
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  try {
    const { channel, failures } = channelUnderTest();
    const primed = channel.prime("upload", "gmu_one");
    const worker = FakeWorker.last!;

    const attached = channel.attachExternal(() => {});
    expect(worker.terminated).toBe(1);

    await channel.teardown(false);
    expect(await attached).toBe("superseded");
    expect(await primed).toBe(false);
    expect(failures).toEqual([]);
  } finally {
    globalThis.Worker = realWorker;
  }
});

// A discarded stage arriving mid-finalize must resolve the pending grace rather
// than start a second one, and terminate the worker exactly once.
test("teardown(false) while finalizing resolves the pending grace", async () => {
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  try {
    const { channel, failures } = channelUnderTest();
    void channel.prime("upload", "gmu_test");
    const worker = FakeWorker.last!;

    const finalizing = channel.teardown(true);
    expect(worker.sent).toContainEqual({ type: "stop" });
    expect(worker.terminated).toBe(0);

    await channel.teardown(false);
    await finalizing;
    expect(worker.terminated).toBe(1);
    expect(failures).toEqual([]);
  } finally {
    globalThis.Worker = realWorker;
  }
});
