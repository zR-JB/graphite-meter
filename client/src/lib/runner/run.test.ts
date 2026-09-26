import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import { stubGlobals } from "../test-helpers.testutil";
import { TEST_BUILD_TOKENS, testPreparedPaths } from "./test-helpers.testutil";
import { DEFAULT_CONFIG } from "../state/defaults";
import type {
  PhaseActivity,
  ReceiverCheckpoint,
  RunResult,
  RunnerConfig,
  RunnerEvent,
} from "./contract";
import type { ParticipantHost, StageTransport } from "./transport";
import { buildHistoryRecord, isHistoryRecord } from "../history/types";
import { ServerAuthenticationRequired } from "../servers/credentials";

let restore: () => void;
const clock = performance.now;
beforeEach(() => {
  restore = stubGlobals(TEST_BUILD_TOKENS);
  jest.useFakeTimers();
});
afterEach(() => {
  performance.now = clock;
  jest.useRealTimers();
  restore();
});

/** Page timers stop while the monotonic clock runs on. */
function suspend(ms: number): void {
  const now = performance.now.bind(performance);
  performance.now = () => now() + ms;
}

async function advance(ms: number): Promise<void> {
  for (let elapsed = 0; elapsed < ms; elapsed += 5) {
    jest.advanceTimersByTime(5);
    for (let i = 0; i < 30; i++) await Promise.resolve();
  }
}

interface Peer {
  id: string;
  /** Bytes per millisecond in each direction. */
  rate?: number;
  latency?: boolean;
  prepare?(
    activity: PhaseActivity,
    host: ParticipantHost,
  ): void | Promise<void>;
  measure?(host: ParticipantHost, activity: PhaseActivity): void;
  finish?(host: ParticipantHost): void | Promise<void>;
  discard?(host: ParticipantHost, incomplete: boolean): void;
  checkpoint?(measuring: boolean): Promise<ReceiverCheckpoint | null>;
}

async function harness(
  peers: Peer[],
  stages: Partial<RunnerConfig["stages"]>,
  duration: Partial<RunnerConfig["duration"]>,
  options: {
    latencySource?: string;
    adaptive?: boolean;
    loadedLatency?: boolean;
  } = {},
) {
  const { Run } = await import("./run");
  const calls: string[] = [];
  const events: RunnerEvent[] = [];
  const servers = peers.map((peer) => {
    const url = `https://${peer.id}.example`;
    const paths = testPreparedPaths();
    paths.throughput.target.origin = paths.throughput.fetch.origin = url;
    if (peer.latency === false) paths.latency = null;
    else paths.latency!.target.origin = url;
    return { server: { id: peer.id, url, name: peer.id }, paths };
  });
  const run = new Run(
    servers,
    options.latencySource ?? peers[0].id,
    ({ host, paths, activity }) => {
      const peer = peers.find(
        (entry) =>
          paths.throughput.target.origin === `https://${entry.id}.example`,
      )!;
      const rate = peer.rate ?? 1;
      const tag = `${peer.id}:${activity.stage}`;
      let measuring = false;
      let started = 0;
      let timer: ReturnType<typeof setInterval> | undefined;
      const receiver = (): ReceiverCheckpoint => {
        const ms = performance.now() - started;
        return {
          id: `${tag}`,
          bytes: Math.round(ms * rate),
          nanos: Math.round(ms * 1e6) + 1,
          receivedAtMs: performance.now(),
        };
      };
      const stop = () => {
        measuring = false;
        clearInterval(timer);
      };
      const stage: StageTransport = {
        async prepare() {
          calls.push(`begin:${tag}`);
          await peer.prepare?.(activity, host);
        },
        async ready() {},
        measure() {
          measuring = true;
          started = performance.now();
          calls.push(`measure:${peer.id}`);
          if (activity.transfer.length)
            timer = setInterval(() => {
              if (activity.transfer.includes("down")) host.download(rate * 20);
              if (
                activity.transfer.includes("up") &&
                (performance.now() - started) % 100 < 20
              )
                host.receiver(receiver());
            }, 20);
          peer.measure?.(host, activity);
        },
        async finish() {
          stop();
          calls.push(`end:${peer.id}`);
          await peer.finish?.(host);
        },
        discard(incomplete = false) {
          stop();
          calls.push(`discard:${peer.id}`);
          peer.discard?.(host, incomplete);
        },
        checkpoint: () =>
          peer.checkpoint?.(measuring) ?? Promise.resolve(receiver()),
      };
      return stage;
    },
  );
  run.on((event) => events.push(event));
  const config: RunnerConfig = {
    ...structuredClone(DEFAULT_CONFIG),
    stages: {
      latency: false,
      download: false,
      upload: false,
      bidirectional: false,
      ...stages,
    },
    skipLoadedLatencyWhenStageOff: !options.loadedLatency,
    duration: {
      warmupMs: 0,
      latencyMs: 0,
      downloadMs: 0,
      uploadMs: 0,
      bidirectionalMs: 0,
      ...duration,
    },
    adaptive: !!options.adaptive,
  };
  const terminal = () =>
    events.find((event) => event.type === "complete" || event.type === "error");
  return {
    run,
    calls,
    events,
    config,
    start: () => run.start(config, 0),
    phases: () =>
      events.flatMap((event) =>
        event.type === "phase" ? [event.transition.to] : [],
      ),
    async result(limitMs = 20_000): Promise<RunResult> {
      for (let t = 0; t < limitMs && !terminal(); t += 50) await advance(50);
      const end = terminal();
      run.dispose();
      if (end?.type === "complete") return end.result;
      throw end?.type === "error" ? end.error : new Error("run did not end");
    },
  };
}
/** Fixture lanes report at the end of each 20 ms period, so rates are within 5%. */
const near = (value: number | undefined, expected: number) =>
  expect(Math.abs((value ?? 0) / expected - 1)).toBeLessThan(0.05);
const two = (a: Partial<Peer> = {}, b: Partial<Peer> = {}): Peer[] => [
  { id: "a", rate: 1, ...a },
  { id: "b", rate: 3, ...b },
];
const fail = (after: number) => (host: ParticipantHost) =>
  setTimeout(() => host.fail("connection-lost", "fixture"), after);
const probe =
  (rttMs: number, count = 1) =>
  (host: ParticipantHost) => {
    for (let i = 0; i < count; i++)
      host.latency({ rttMs, timedOut: false, observedAtMs: performance.now() });
  };

test("warmup probes never enter a measured population", async () => {
  const h = await harness(
    [
      {
        id: "self",
        prepare: (_activity, host) =>
          void setTimeout(() => probe(900, 5)(host), 50),
        measure: probe(10, 4),
      },
    ],
    { latency: true },
    { warmupMs: 200, latencyMs: 400 },
  );
  h.start();
  const result = await h.result();
  expect(result.latencyByStage.latency).toMatchObject({
    probeCount: 4,
    maxMs: 10,
  });
});

test("one server runs every stage in order and its saved record describes the run", async () => {
  const h = await harness(
    [{ id: "self", rate: 2, measure: probe(10, 4) }],
    { latency: true, download: true, upload: true },
    {
      warmupMs: 200,
      latencyMs: 400,
      downloadMs: 1_000,
      uploadMs: 1_000,
    },
  );
  h.start();
  const result = await h.result();
  expect(h.phases()).toEqual([
    "connecting",
    "warmup",
    "latency",
    "warmup",
    "download",
    "warmup",
    "upload",
  ]);
  // The warmup-to-measure seam keeps its stage resources.
  expect(h.calls).toEqual([
    "begin:self:latency",
    "measure:self",
    "end:self",
    "begin:self:download",
    "measure:self",
    "end:self",
    "begin:self:upload",
    "measure:self",
    "end:self",
  ]);
  expect(result.latency?.reportedMs).toBe(10);
  near(result.download?.reportedBytesPerSec, 2_000);
  near(result.upload?.reportedBytesPerSec, 2_000);
  expect(result.outcome).toBe("complete");
  expect(result.multiServer.participants).toEqual(["self"]);
  expect(h.events.filter((event) => event.type === "complete")).toHaveLength(1);
  const saved = buildHistoryRecord(result, { paths: null, clientBuild: "t" });
  expect(isHistoryRecord(JSON.parse(JSON.stringify(saved)))).toBe(true);
});

test("two servers sum their windows, and terminal evidence after the final boundary changes nothing", async () => {
  const late = { finish: (host: ParticipantHost) => host.download(1_000_000) };
  const h = await harness(
    two(late, late),
    { download: true, upload: true },
    { downloadMs: 1_400, uploadMs: 1_400 },
  );
  h.start();
  const result = await h.result();
  near(result.download?.reportedBytesPerSec, 4_000);
  near(result.upload?.reportedBytesPerSec, 4_000);
  expect(result.download!.totalBytes).toBeLessThan(10_000);
  near(result.multiServer.servers[0].download?.reportedBytesPerSec, 1_000);
  near(result.multiServer.servers[1].download?.reportedBytesPerSec, 3_000);
});

test("a late dropout leaves the headline unavailable while the failed stage keeps earlier evidence", async () => {
  const h = await harness(
    two({ measure: fail(900) }),
    { download: true },
    { downloadMs: 1_400 },
  );
  h.start();
  const result = await h.result();
  expect(result.download).toBeNull();
  expect(result.outcome).toBe("incomplete");
  expect(result.multiServer.participants).toEqual(["b"]);
  expect(
    result.multiServer.intervals.map((interval) => interval.reason),
  ).toEqual(["stage-start", "dropout"]);
  near(result.multiServer.intervals[0].full?.downBytesPerSec ?? 0, 4_000);
  near(result.multiServer.servers[0].download?.reportedBytesPerSec, 1_000);
  expect(result.multiServer.failures).toMatchObject([
    { serverId: "a", stage: "download", scope: "throughput" },
  ]);
});

test("several servers that all fail end the run; a sole server skips to its next stage", async () => {
  let downloadFailures = 0;
  const drop = {
    measure: (host: ParticipantHost, activity: PhaseActivity) => {
      if (activity.stage === "download")
        setTimeout(
          () => (downloadFailures++, host.fail("connection-lost", "x")),
          400,
        );
    },
  };
  const h = await harness(
    two(drop, drop),
    { download: true, upload: true },
    { downloadMs: 1_400, uploadMs: 1_400 },
  );
  h.start();
  const result = await h.result();
  expect(downloadFailures).toBe(2);
  expect(h.phases()).not.toContain("aborted");
  expect(h.phases()).not.toContain("upload");
  expect(result.stages).toMatchObject({ download: "failed", upload: "failed" });
  expect(result.outcome).toBe("incomplete");
  expect(result.multiServer.participants).toEqual([]);
  expect(h.events.filter((event) => event.type === "complete")).toHaveLength(1);

  const sole = await harness(
    [{ id: "self", ...drop }],
    {
      download: true,
      upload: true,
    },
    { downloadMs: 1_400, uploadMs: 1_400 },
  );
  sole.start();
  const skipped = await sole.result();
  expect(skipped.stages).toMatchObject({
    download: "failed",
    upload: "complete",
  });
  expect(skipped.outcome).toBe("incomplete");
});

test("a latency-only failure keeps both throughput participants", async () => {
  const h = await harness(
    two({ measure: (host) => setTimeout(() => host.latencyIncomplete(), 200) }),
    { download: true },
    { downloadMs: 1_400 },
    { loadedLatency: true },
  );
  h.start();
  const result = await h.result();
  expect(result.multiServer.participants).toEqual(["a", "b"]);
  near(result.download?.reportedBytesPerSec, 4_000);
  expect(result.multiServer.failures).toMatchObject([
    { serverId: "a", scope: "latency" },
  ]);
  expect(result.outcome).toBe("partial");
});

test("a throughput dropout reports discarded loaded probes before the participant is reduced", async () => {
  const h = await harness(
    two(
      {
        measure: (host) => {
          probe(10)(host);
          fail(200)(host);
        },
        discard: (host, incomplete) => incomplete && host.latencyIncomplete(),
      },
      { measure: probe(10) },
    ),
    { download: true },
    { downloadMs: 1_400 },
    { loadedLatency: true },
  );
  h.start();
  const result = await h.result();
  const [failed, healthy] = result.multiServer.servers;
  expect(failed.latencyByStage.download).toMatchObject({
    accountingComplete: false,
    probeCount: 1,
  });
  expect(healthy.latencyByStage.download).toMatchObject({
    accountingComplete: true,
    probeCount: 1,
  });
  near(result.download?.reportedBytesPerSec, 3_000);
});

test("a server that cannot prepare is dropped; the run fails only when none survive", async () => {
  const refuse = { prepare: () => Promise.reject(new Error("fixture")) };
  const early = await harness(
    two(refuse),
    { download: true },
    {
      downloadMs: 1_000,
    },
  );
  early.start();
  expect(early.phases()).toEqual(["connecting"]);
  const survived = await early.result();
  near(survived.download?.reportedBytesPerSec, 3_000);
  expect(survived.outcome).toBe("partial");
  expect(survived.multiServer.failures).toMatchObject([
    { serverId: "a", stage: "download", reason: "preparation-failed" },
  ]);
  const none = await harness(
    two(refuse, refuse),
    { download: true },
    {
      downloadMs: 1_000,
    },
  );
  none.start();
  await expect(none.result()).rejects.toMatchObject({
    reason: "preparation-failed",
    message: expect.stringMatching(/^All selected servers failed/),
  });
  for (const [cause, reason] of [
    [new TypeError("Failed to fetch"), "connection-lost"],
    [new DOMException("late", "TimeoutError"), "timeout"],
  ] as const) {
    const reject = {
      prepare: () => Promise.reject(new Error("no", { cause })),
    };
    const refused = await harness(
      two(reject, reject),
      { download: true },
      {
        downloadMs: 1_000,
      },
    );
    refused.start();
    await expect(refused.result()).rejects.toMatchObject({ reason });
  }

  const later = await harness(
    two({
      prepare: (activity) =>
        activity.stage === "upload"
          ? Promise.reject(new Error("fixture"))
          : undefined,
    }),
    { download: true, upload: true },
    { downloadMs: 1_400, uploadMs: 1_400 },
  );
  later.start();
  const result = await later.result();
  near(result.download?.reportedBytesPerSec, 4_000);
  near(result.upload?.reportedBytesPerSec, 3_000);
  expect(result.multiServer.failures).toMatchObject([
    { serverId: "a", stage: "upload", reason: "preparation-failed" },
  ]);
});

test("the headline latency source is fixed before the run and is not pooled", async () => {
  const h = await harness(
    [
      { id: "a", latency: false },
      { id: "b", measure: probe(2, 4) },
      { id: "c", measure: probe(40, 4) },
    ],
    { latency: true, download: true },
    { latencyMs: 50, downloadMs: 1_000 },
    { latencySource: "c" },
  );
  h.start();
  const result = await h.result();
  expect(h.calls.filter((call) => call.startsWith("begin:"))).toEqual([
    "begin:b:latency",
    "begin:c:latency",
    "begin:a:download",
    "begin:b:download",
    "begin:c:download",
  ]);
  expect(result.latency?.reportedMs).toBe(40);
  expect(result.multiServer.latencyFocus).toBe("c");
  expect(result.multiServer.servers[0].latencyByStage.latency).toBeNull();
  expect(result.multiServer.servers[1].latency?.reportedMs).toBe(2);
});

test("a stable feed completes early and each result arrives before the next stage", async () => {
  const h = await harness(
    two(),
    { download: true, upload: true },
    { downloadMs: 6_000, uploadMs: 6_000 },
    { adaptive: true },
  );
  h.start();
  const started = performance.now();
  const result = await h.result();
  expect(performance.now() - started).toBeLessThan(11_000);
  expect(result.stages.download).toBe("complete");
  near(result.upload?.reportedBytesPerSec, 4_000);
  const stageResult = h.events.findIndex(
    (event) => event.type === "stageResult" && event.stage === "download",
  );
  const upload = h.events.findIndex(
    (event) => event.type === "phase" && event.transition.to === "upload",
  );
  expect(stageResult).toBeGreaterThan(-1);
  expect(stageResult).toBeLessThan(upload);
});

test("a conflicting live stream plan is rejected without changing the running schedule", async () => {
  const h = await harness(
    [{ id: "self", rate: 3, latency: false }],
    { download: true },
    { downloadMs: 1_200 },
  );
  h.config.transferStreams = { mode: "forced", count: 5 };
  h.start();
  await advance(100);
  const { stages, duration, adaptive } = h.config;
  expect(() =>
    h.run.reconfigure({
      stages: { ...stages, upload: true },
      duration,
      adaptive,
    }),
  ).toThrow("Forced streams");
  const result = await h.result();
  near(result.download?.reportedBytesPerSec, 3_000);
  expect(result.upload).toBeNull();
});

test("a missed final checkpoint keeps the interval and its headline", async () => {
  const h = await harness(
    two({ checkpoint: async () => null }),
    { upload: true },
    {
      uploadMs: 1_400,
    },
  );
  h.start();
  const result = await h.result();
  near(result.upload?.reportedBytesPerSec, 4_000);
  expect(result.multiServer.intervals).toHaveLength(1);
  expect(result.multiServer.failures).toEqual([]);
});
const receiverOf = (id: string): ReceiverCheckpoint => ({
  id,
  bytes: 0,
  nanos: 1,
  receivedAtMs: 0,
});

test("an expired grant at the final checkpoint asks for sign-in and removes only that server", async () => {
  const server = { id: "a", name: "a", url: "https://a.example" };
  const h = await harness(
    two({
      checkpoint: () =>
        Promise.reject(new ServerAuthenticationRequired(server)),
    }),
    { upload: true, download: true },
    { uploadMs: 1_000, downloadMs: 1_000 },
  );
  h.start();
  const result = await h.result();
  expect(result.multiServer.failures).toMatchObject([
    {
      serverId: "a",
      stage: "upload",
      reason: "sign-in-required",
      message: "Sign in to a",
    },
  ]);
  expect(result.multiServer.participants).toEqual(["b"]);
});

test("each server ends its stage as soon as its own final evidence arrives", async () => {
  let slow = false;
  const h = await harness(
    two({
      checkpoint: async () => {
        if (slow) await new Promise((resolve) => setTimeout(resolve, 1_000));
        return receiverOf("a");
      },
      measure: () => setTimeout(() => (slow = true), 1_050),
    }),
    { upload: true },
    { uploadMs: 1_100 },
  );
  h.start();
  await advance(1_400);
  expect(h.calls.filter((call) => call.startsWith("end:"))).toEqual(["end:b"]);
  await h.result();
});

test("an aborted stage end delivers no late events", async () => {
  let release!: () => void;
  const hold = {
    finish: () => new Promise<void>((resolve) => (release = resolve)),
  };
  const h = await harness(
    two(hold, hold),
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  await advance(1_100);
  expect(h.calls).toContain("end:a");
  h.run.abort();
  const count = h.events.length;
  expect(h.events.at(-1)).toMatchObject({
    type: "phase",
    transition: { to: "aborted" },
  });
  release();
  await advance(500);
  expect(h.events).toHaveLength(count);
  h.run.dispose();
});

test("evidence that stops late in a stage fails that stage, and a sole server stalls the run", async () => {
  const stall = (host: ParticipantHost, activity: PhaseActivity) => {
    if (activity.stage === "download")
      setTimeout(
        () => host.stall({ reason: "connection-lost", detail: "quiet" }),
        700,
      );
  };
  const h = await harness(
    two({ measure: stall }),
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  const result = await h.result();
  expect(result.multiServer.failures).toMatchObject([
    { serverId: "a", stage: "download", message: "quiet" },
  ]);
  expect(result.multiServer.participants).toEqual(["b"]);
  expect(h.events.some((event) => event.type === "stall")).toBe(false);

  // Evidence silent past the recovery window leaves the interval within the stage.
  const early = await harness(
    two({ measure: stall }),
    { download: true },
    { downloadMs: 3_500 },
  );
  early.start();
  const dropped = await early.result();
  const [, survivors] = dropped.multiServer.intervals;
  expect(survivors).toMatchObject({ reason: "dropout", participants: ["b"] });
  expect(survivors.startMs).toBeLessThan(2_300);
  near(dropped.download?.reportedBytesPerSec, 3_000);

  const sole = await harness(
    [{ id: "self", measure: stall }],
    { download: true },
    { downloadMs: 2_000 },
  );
  sole.start();
  await advance(900);
  expect(sole.events.filter((event) => event.type === "stall")).toHaveLength(1);
  expect(
    sole.events.findLast((event) => event.type === "progress"),
  ).toMatchObject({ measuring: false });
  await sole.result();
});

test("a suspended page enters every segment in order and never folds the gap into evidence", async () => {
  const frozen = await harness(
    [{ id: "self" }],
    { download: true },
    {
      downloadMs: 4_000,
    },
  );
  frozen.start();
  await advance(500);
  suspend(4_000);
  const lost = await frozen.result();
  expect(lost.stages.download).toBe("failed");
  expect(lost.multiServer.failures).toMatchObject([
    { stage: "download", reason: "insufficient-evidence" },
  ]);
  expect(lost.multiServer.intervals.map((i) => i.reason)).toEqual([
    "stage-start",
    "evidence-resumed",
  ]);

  const h = await harness(
    [{ id: "self" }],
    { download: true, upload: true },
    { warmupMs: 200, downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  await advance(20);
  // One long gap never skips a segment.
  suspend(5_000);
  const result = await h.result();
  expect(h.phases()).toEqual([
    "connecting",
    "warmup",
    "download",
    "warmup",
    "upload",
  ]);
  expect(result.outcome).toBe("complete");
});
