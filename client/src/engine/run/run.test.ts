import { expect, spyOn, test } from "bun:test";
import { SignInRequired, StageFailure, type StageEvents } from "../types";
import { incoherence, type RunResult } from "./result";
import { Run, type RunEvent, type RunPlan } from "./run";
import type { Activity, Stage } from "./schedule";
import { FakeClock, prepared, resources, type Peer } from "./fakes.testutil";

function harness(
  peers: Peer[],
  stages: Partial<RunPlan["stages"]>,
  duration: Partial<RunPlan["duration"]>,
  options: {
    focus?: string;
    adaptive?: boolean;
    loadedLatency?: boolean;
    deal?: (activity: Activity) => void;
  } = {},
) {
  const clock = new FakeClock();
  const calls: string[] = [];
  const events: RunEvent[] = [];
  const plan: RunPlan = {
    stages: { latency: false, download: false, upload: false, bidirectional: false, ...stages },
    duration: {
      warmupMs: 0,
      latencyMs: 0,
      downloadMs: 0,
      uploadMs: 0,
      bidirectionalMs: 0,
      ...duration,
    },
    skipLoadedLatencyWhenStageOff: !options.loadedLatency,
    pingCadence: "reply-driven",
    loadedPingCadence: "medium",
    adaptive: !!options.adaptive,
  };
  let run: Run | undefined;
  const terminal = () =>
    events.find((event) => event.type === "complete" || event.type === "error");
  return {
    clock,
    calls,
    events,
    plan,
    get run(): Run {
      return run!;
    },
    start() {
      run = new Run({
        servers: peers.map((peer) => prepared(peer.id, peer.latency !== false, peer.signedOut)),
        focus: options.focus ?? peers[0]!.id,
        dropped: [],
        plan,
        open: (setup) =>
          resources(peers.find(({ id }) => id === setup.server.server.id)!, setup, calls, clock),
        streams: (servers, activity) => {
          options.deal?.(activity);
          return new Map(
            servers.map(({ server }) => [
              server.id,
              {
                streams: {
                  down: +activity.transfer.includes("down"),
                  up: +activity.transfer.includes("up"),
                },
              },
            ]),
          );
        },
        clock,
      });
      run.on((event) => events.push(event));
      run.start();
    },
    elapse: (ms: number) => clock.advance(ms),
    phases: () =>
      events.flatMap((event, index) =>
        event.type === "phase" &&
        event.phase !== events.slice(0, index).findLast((each) => each.type === "phase")?.phase
          ? [event.phase]
          : [],
      ),
    async result(limitMs = 30_000): Promise<RunResult> {
      for (let t = 0; t < limitMs && !terminal(); t += 50) await clock.advance(50);
      const end = terminal();
      if (end?.type === "complete") {
        expect(incoherence(end.result, end.plans)).toEqual([]);
        return end.result;
      }
      throw end?.type === "error" ? end : new Error("the run did not end");
    },
  };
}

/** Lanes report at the end of each 20 ms period, so rates are within 5%. */
const near = (value: number | undefined, expected: number) =>
  expect(Math.abs((value ?? 0) / expected - 1)).toBeLessThan(0.05);
const two = (a: Partial<Peer> = {}, b: Partial<Peer> = {}): Peer[] => [
  { id: "a", rate: 1, ...a },
  { id: "b", rate: 3, ...b },
];
let clock: FakeClock;
const later = (ms: number, fn: () => void) => void clock.after(ms, fn);
const fail = (ms: number) => (events: StageEvents) =>
  later(ms, () => events.failed("connection-lost", "fixture"));
const probe =
  (rttMs: number, count = 1) =>
  (events: StageEvents) =>
    events.probes(
      Array.from({ length: count }, () => ({
        rttMs,
        timedOut: false,
        handlingMs: null,
        observedAt: clock.t,
        inWindow: true,
      })),
    );
/** Every direction of the named stage moves nothing from `from` to `to` ms into its window. */
const hole = (stage: Stage, from: number, to: number) => (activity: Activity, ms: number) =>
  activity.stage === stage && ms >= from && ms < to;

function setup(...args: Parameters<typeof harness>) {
  const h = harness(...args);
  clock = h.clock;
  return h;
}

test("warmup probes never enter a measured population", async () => {
  const h = setup(
    [
      {
        id: "self",
        prepare: (_, events) => later(50, () => probe(900, 5)(events)),
        measure: probe(10, 4),
      },
    ],
    { latency: true },
    { warmupMs: 200, latencyMs: 400 },
  );
  h.start();
  expect((await h.result()).latencyByStage.latency).toMatchObject({ probeCount: 4, maxMs: 10 });
});

test("loaded latency ends with the stage's window, however late its final checkpoint answers", async () => {
  // A probe every 80 ms: 10 ms replies through the 1 s upload window, 200 ms after it, and a final checkpoint taking 600 ms.
  const measure = (events: StageEvents) => {
    const start = clock.t;
    clock.every(80, () => {
      const late = clock.t - start > 1_000;
      events.probes([
        {
          rttMs: late ? 200 : 10,
          timedOut: false,
          handlingMs: null,
          observedAt: clock.t,
          inWindow: true,
        },
      ]);
    });
  };
  const h = setup(
    [{ id: "self", measure, checkpointLagMs: 600 }],
    { upload: true },
    { uploadMs: 1_000 },
    { loadedLatency: true },
  );
  h.start();
  const loaded = (await h.result()).latencyByStage.upload;
  expect(loaded).toMatchObject({ p90Ms: 10, maxMs: 10 });
});

test("replies delivered in separate batches stay adjacent for jitter", async () => {
  const measure = (events: StageEvents) => {
    for (const rttMs of [10, 20, 10]) {
      probe(rttMs)(events);
      events.latencyResumed();
    }
  };
  const h = setup([{ id: "self", measure }], { latency: true }, { latencyMs: 400 });
  h.start();
  expect((await h.result()).latencyByStage.latency).toMatchObject({ jitterMs: 10 });
});

test("an aborted latency stage keeps its summary after the run releases its replies", async () => {
  const h = setup([{ id: "self", measure: probe(10, 4) }], { latency: true }, { latencyMs: 1_000 });
  h.start();
  await h.elapse(300);
  h.run.abort();
  expect(h.events.at(-1)).toMatchObject({ type: "aborted" });
  expect(h.run.details().servers[0]!.latencyByStage.latency).toMatchObject({
    probeCount: 4,
    p50Ms: 10,
  });
});

test("a warmup moves the live rate, which no result counts, and the window opens on that rate", async () => {
  // 3 bytes per ms through the 800 ms warmup, then 1 per ms; the live rate shows 500 ms after its first delivery, within the warmup.
  let warming = () => {};
  const h = setup(
    [
      {
        id: "self",
        rate: 1,
        prepare: (_, events) => void (warming = clock.every(20, () => events.warmupDownload(60))),
        measure: () => warming(),
      },
    ],
    { download: true },
    { warmupMs: 800, downloadMs: 1_000 },
  );
  h.start();
  const result = await h.result();
  const samples = h.events.flatMap((event) => (event.type === "live" ? [event.sample] : []));
  const warm = samples.filter((sample) => sample.warmup);
  expect(warm.length).toBeGreaterThan(2);
  // Before the live rate has its evidence, the warmup's ramp already reads.
  expect(warm.some((sample) => sample.t < 300 && (sample.down ?? 0) > 0)).toBe(true);
  near(warm.at(-1)!.down!, 3_000);
  expect(warm.every((sample) => sample.bytes === 0)).toBe(true);
  // The first window sample is already moving: the rate carried over, nothing started from empty.
  expect(samples.find((sample) => !sample.warmup)!.down).toBeGreaterThan(0);
  near(result.download?.reportedBytesPerSec, 1_000);
});

test("one server runs every stage in order on one set of resources per stage", async () => {
  const h = setup(
    [{ id: "self", rate: 2, measure: probe(10, 4) }],
    { latency: true, download: true, upload: true },
    { warmupMs: 200, latencyMs: 400, downloadMs: 1_000, uploadMs: 1_000 },
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
  expect(h.calls).toEqual(
    ["latency", "download", "upload"].flatMap((stage) => [
      `begin:self:${stage}`,
      "measure:self",
      "end:self",
    ]),
  );
  expect(result.latency?.reportedMs).toBe(10);
  near(result.download?.reportedBytesPerSec, 2_000);
  near(result.upload?.reportedBytesPerSec, 2_000);
  expect(result.outcome).toBe("complete");
  expect(JSON.parse(JSON.stringify(result))).toEqual(result);
  expect(result.download?.wire?.totalMultiplier).toBeGreaterThan(1);
});

test("two servers sum their windows, and terminal evidence after the final boundary changes nothing", async () => {
  const late = { finish: (events: StageEvents) => events.download(1_000_000) };
  const h = setup(
    two(late, late),
    { download: true, upload: true },
    { downloadMs: 1_400, uploadMs: 1_400 },
  );
  h.start();
  const result = await h.result();
  near(result.download?.reportedBytesPerSec, 4_000);
  near(result.upload?.reportedBytesPerSec, 4_000);
  expect(result.download!.totalBytes).toBeLessThan(10_000);
  near(result.multiServer.servers[0]!.download?.reportedBytesPerSec, 1_000);
  near(result.multiServer.servers[1]!.download?.reportedBytesPerSec, 3_000);
});

test("bytes a lane counted before the window closed but had not reported yet count at the final boundary", async () => {
  const h = setup(
    [
      {
        id: "self",
        rate: 1,
        flush: (events) => events.download(250),
        finish: (events) => events.download(1_000_000),
      },
    ],
    { download: true },
    { downloadMs: 1_000 },
  );
  h.start();
  const result = await h.result();
  expect(result.download!.totalBytes).toBe(1_250);
  near(result.download?.reportedBytesPerSec, 1_250);
});

test("lanes that read on until their flush answers count against the time it answered", async () => {
  const h = setup(
    [{ id: "self", rate: 1, flush: () => new Promise<void>((resolve) => later(200, resolve)) }],
    { download: true },
    { downloadMs: 1_000 },
  );
  h.start();
  near((await h.result()).download?.reportedBytesPerSec, 1_000);
});

test("an answer that reaches the page late still closes the boundary when its count was read", async () => {
  // 1 byte per ms, counted until the flush reads the count; the answer arrives 200 ms after that.
  let counting = true;
  const h = setup(
    [
      {
        id: "self",
        rate: 0,
        measure: (events) => clock.every(20, () => counting && events.download(20)),
        flush: () => {
          counting = false;
          const read = clock.t;
          return new Promise<number>((resolve) => later(200, () => resolve(read)));
        },
      },
    ],
    { download: true },
    { downloadMs: 1_000 },
  );
  h.start();
  near((await h.result()).download?.reportedBytesPerSec, 1_000);
});

test("servers whose lanes answer the final flush at different times are cut at the same moment", async () => {
  const late = () => new Promise<void>((resolve) => later(200, resolve));
  const h = setup(
    two({ flush: () => {} }, { rate: 1, flush: late }),
    { download: true },
    { downloadMs: 1_000 },
  );
  h.start();
  const result = await h.result();
  near(result.download?.reportedBytesPerSec, 2_000);
  for (const server of result.multiServer.servers)
    near(server.download?.reportedBytesPerSec, 1_000);
});

test("a server that stayed without a result of its own makes the stage partial, coherently", async () => {
  const h = setup(two({}, { rate: 0 }), { download: true }, { downloadMs: 1_000 });
  h.start();
  const result = await h.result();
  expect([result.stages.download, result.outcome]).toEqual(["partial", "partial"]);
  expect(result.multiServer.failures).toMatchObject([
    { serverId: "b", reason: "insufficient-evidence" },
  ]);
});

test("a server lost in the latency stage is not prepared for the later stages", async () => {
  const lose = (how: (events: StageEvents) => unknown): Partial<Peer> => ({
    measure: (events, { stage }) => void (stage === "latency" && how(events)),
  });
  const h = setup(
    [
      ...two(lose(fail(100))),
      { id: "c", ...lose((events) => events.latencyStalled("fixture")) },
      {
        id: "d",
        prepare: ({ stage }) =>
          stage === "latency"
            ? Promise.reject(new StageFailure("path-failed", "lapsed"))
            : undefined,
      },
    ],
    { latency: true, download: true },
    { latencyMs: 400, downloadMs: 1_000 },
  );
  h.start();
  const { multiServer, stages } = await h.result();
  for (const id of ["a", "c", "d"]) expect(h.calls).not.toContain(`begin:${id}:download`);
  expect(multiServer.participants).toEqual(["b"]);
  expect(multiServer.failures).toMatchObject([
    { serverId: "d", stage: "latency", scope: "latency", reason: "path-failed" },
    { serverId: "a", stage: "latency", scope: "latency" },
    { serverId: "c", stage: "latency", scope: "latency" },
  ]);
  expect(stages.download).toBe("complete");
});

test("server failure text is bounded and dropped when it could disguise itself", async () => {
  const say = (message: string) => (events: StageEvents) =>
    later(100, () => events.failed("protocol-error", message));
  const h = setup(
    [
      { id: "a", measure: say("paid \u202eevil") },
      { id: "b", measure: say("x".repeat(300)) },
      { id: "c" },
    ],
    { download: true },
    { downloadMs: 1_400 },
  );
  h.start();
  const { failures } = (await h.result()).multiServer;
  expect(failures.map(({ message }) => message)).toEqual(["", "x".repeat(256)]);
});

test("several servers that all fail end the run; a sole server skips to its next stage", async () => {
  const drop = {
    measure: (events: StageEvents, { stage }: Activity) => {
      if (stage === "download") later(400, () => events.failed("connection-lost", "x"));
    },
  };
  const h = setup(
    two(drop, drop),
    { download: true, upload: true },
    { downloadMs: 1_400, uploadMs: 1_400 },
  );
  h.start();
  const result = await h.result();
  expect(h.phases()).not.toContain("upload");
  expect(result.stages).toMatchObject({ download: "failed", upload: "failed" });
  expect(
    result.multiServer.failures.map(({ serverId, stage, reason }) => [serverId, stage, reason]),
  ).toEqual([
    ["a", "download", "connection-lost"],
    ["b", "download", "connection-lost"],
    ["a", "upload", "connection-lost"],
    ["b", "upload", "connection-lost"],
  ]);
  expect(result.outcome).toBe("incomplete");
  expect(result.multiServer.participants).toEqual([]);
  expect(h.events.filter(({ type }) => type === "complete" || type === "aborted")).toHaveLength(1);

  const sole = setup(
    [{ id: "self", ...drop }],
    { download: true, upload: true },
    { downloadMs: 1_400, uploadMs: 1_400 },
  );
  sole.start();
  const skipped = await sole.result();
  expect(skipped.stages).toMatchObject({ download: "failed", upload: "complete" });
  expect(skipped.outcome).toBe("incomplete");
});

test("a latency-only failure keeps both throughput participants", async () => {
  const h = setup(
    two({ measure: (events) => later(200, () => events.probesIncomplete()) }),
    { download: true },
    { downloadMs: 1_400 },
    { loadedLatency: true },
  );
  h.start();
  const result = await h.result();
  expect(result.multiServer.participants).toEqual(["a", "b"]);
  near(result.download?.reportedBytesPerSec, 4_000);
  expect(result.multiServer.failures).toMatchObject([{ serverId: "a", scope: "latency" }]);
  expect(result.outcome).toBe("partial");
});

test("a throughput dropout reports discarded loaded probes before the server is reduced", async () => {
  const h = setup(
    two(
      {
        measure: (events) => {
          probe(10)(events);
          fail(200)(events);
        },
        discard: (events, incomplete) => incomplete && events.probesIncomplete(),
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
  expect(failed!.latencyByStage.download).toMatchObject({
    accountingComplete: false,
    probeCount: 1,
  });
  expect(healthy!.latencyByStage.download).toMatchObject({
    accountingComplete: true,
    probeCount: 1,
  });
  near(result.download?.reportedBytesPerSec, 3_000);
});

test("a server that cannot prepare is dropped; the run fails only when none survive", async () => {
  const refuse = { prepare: () => Promise.reject(new Error("fixture")) };
  const early = setup(two(refuse), { download: true }, { downloadMs: 1_000 });
  early.start();
  expect(early.phases()).toEqual(["connecting"]);
  const survived = await early.result();
  near(survived.download?.reportedBytesPerSec, 3_000);
  expect(survived.outcome).toBe("partial");
  expect(survived.multiServer.failures).toMatchObject([
    { serverId: "a", stage: "download", reason: "path-failed" },
  ]);

  const none = setup(two(refuse, refuse), { download: true }, { downloadMs: 1_000 });
  none.start();
  await expect(none.result()).rejects.toMatchObject({
    reason: "path-failed",
    message: expect.stringMatching(/^All selected test servers failed/),
  });
  for (const [cause, reason] of [
    [new TypeError("Failed to fetch"), "connection-lost"],
    // A lapse is not a channel that stopped delivering.
    [new DOMException("late", "TimeoutError"), "path-failed"],
    [new StageFailure("client-limit", "refused"), "client-limit"],
  ] as const) {
    const reject = { prepare: () => Promise.reject(new Error("no", { cause })) };
    const refused = setup(two(reject, reject), { download: true }, { downloadMs: 1_000 });
    refused.start();
    await expect(refused.result()).rejects.toMatchObject({ reason });
  }

  const upload = setup(
    two({
      prepare: ({ stage }) =>
        stage === "upload" ? Promise.reject(new Error("fixture")) : undefined,
    }),
    { download: true, upload: true },
    { downloadMs: 1_400, uploadMs: 1_400 },
  );
  upload.start();
  const result = await upload.result();
  near(result.download?.reportedBytesPerSec, 4_000);
  near(result.upload?.reportedBytesPerSec, 3_000);
  expect(result.multiServer.failures).toMatchObject([
    { serverId: "a", stage: "upload", reason: "path-failed" },
  ]);
});

test("the headline latency source is fixed before the run and is not pooled", async () => {
  const h = setup(
    [
      { id: "a", latency: false },
      { id: "b", measure: probe(2, 4) },
      { id: "c", measure: probe(40, 4) },
    ],
    { latency: true, download: true },
    { latencyMs: 50, downloadMs: 1_000 },
    { focus: "c" },
  );
  h.start();
  const result = await h.result();
  expect(h.calls).not.toContain("begin:a:latency");
  expect(result.latency?.reportedMs).toBe(40);
  expect(result.multiServer.latencyFocus).toBe("c");
  expect(result.multiServer.servers[0]!.latencyByStage.latency).toBeNull();
  expect(result.multiServer.servers[1]!.latency?.reportedMs).toBe(2);
});

test("a focus server lost in the latency stage hands the headline to a survivor", async () => {
  const h = setup(
    [
      { id: "b", measure: probe(2, 4) },
      { id: "c", measure: (events) => (probe(40, 4)(events), fail(10)(events)) },
    ],
    { latency: true, download: true },
    { latencyMs: 50, downloadMs: 1_000 },
    { focus: "c" },
  );
  h.start();
  const result = await h.result();
  expect(result.latency?.reportedMs).toBe(2);
  expect(result.multiServer.latencyFocus).toBe("b");
  expect(result.stages).toMatchObject({ latency: "partial" });
  expect(result.outcome).toBe("partial");
});

test("a stable feed completes early, and each stage's result arrives before the next stage shows", async () => {
  const h = setup(
    two(),
    { download: true, upload: true },
    { downloadMs: 6_000, uploadMs: 6_000 },
    { adaptive: true },
  );
  h.start();
  const result = await h.result();
  expect(result.durationMs).toBeLessThan(11_000);
  expect(result.stages).toMatchObject({ download: "complete", upload: "complete" });
  near(result.upload?.reportedBytesPerSec, 4_000);
  const ended = h.events.findIndex(
    (event) => event.type === "stageEnd" && event.stage === "download",
  );
  const upload = h.events.findIndex((event) => event.type === "phase" && event.stage === "upload");
  expect(ended).toBeGreaterThan(-1);
  expect(ended).toBeLessThan(upload);
});

test("every stage end reports the status the result keeps, one-lane bidirectional included", async () => {
  const h = setup(
    [{ id: "self", rate: 2, receives: false }],
    { download: true, bidirectional: true },
    {
      downloadMs: 1_000,
      bidirectionalMs: 1_000,
    },
  );
  h.start();
  const result = await h.result();
  expect(result.stages.bidirectional).toBe("failed");
  expect(
    h.events.flatMap((event) => (event.type === "stageEnd" ? [[event.stage, event.status]] : [])),
  ).toEqual([
    ["download", "complete"],
    ["bidirectional", "failed"],
  ]);
  // Without a receiver no boundary is complete, so neither lane has a window.
  expect(result.bidirectional).toEqual({ down: null, up: null, wire: null });
});

test("each server's stage statuses follow the run's rule on its own lanes and failures", async () => {
  const h = setup(
    [
      {
        id: "a",
        measure: (events, { stage }) => {
          if (stage === "latency") probe(10, 4)(events);
          if (stage === "upload") fail(300)(events);
        },
      },
      { id: "b", rate: 3, latency: false },
    ],
    { latency: true, download: true, upload: true },
    { latencyMs: 400, downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  const { multiServer } = await h.result();
  expect(multiServer.servers.map(({ stages }) => stages)).toEqual([
    { latency: "complete", download: "complete", upload: "failed", bidirectional: "not-run" },
    { latency: "not-run", download: "complete", upload: "complete", bidirectional: "not-run" },
  ]);
});

test("a server that left the run reports the stages it never began as not run, with no failure for them", async () => {
  const h = setup(
    two({ measure: (events, { stage }) => void (stage === "download" && fail(100)(events)) }),
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  const { multiServer, stages } = await h.result();
  expect(stages.upload).toBe("complete");
  expect(multiServer.failures.map(({ serverId, stage }) => [serverId, stage])).toEqual([
    ["a", "download"],
  ]);
  expect(multiServer.servers.map(({ stages }) => [stages.download, stages.upload])).toEqual([
    ["failed", "not-run"],
    ["complete", "complete"],
  ]);
});

test("ending for sign-in keeps what was measured and names the reason for each unfinished stage", async () => {
  const h = setup(
    [{ id: "self", rate: 2 }],
    { download: true, upload: true },
    { downloadMs: 2_000, uploadMs: 1_000 },
  );
  h.start();
  await h.elapse(1_300);
  h.run.end("sign-in-required", "Signed out");
  const result = await h.result();
  near(result.download?.reportedBytesPerSec, 2_000);
  expect(result.stages).toMatchObject({ download: "partial", upload: "failed" });
  expect(result.multiServer.failures.map(({ stage, reason }) => [stage, reason])).toEqual([
    ["download", "sign-in-required"],
    ["upload", "sign-in-required"],
  ]);
});

test("ending while the first stage prepares completes the run instead of failing it", async () => {
  const h = setup(
    two(
      { prepare: () => new Promise<void>(() => {}) },
      { prepare: () => new Promise<void>(() => {}) },
    ),
    { download: true },
    { downloadMs: 1_000 },
  );
  h.start();
  await h.elapse(100);
  h.run.end("sign-in-required", "Signed out");
  const result = await h.result();
  expect(result.stages.download).toBe("failed");
  expect(result.outcome).toBe("incomplete");
  expect(h.events.some(({ type }) => type === "error")).toBe(false);
});

test("a 0 ms stage is not planned, and a plan without a stage is refused", async () => {
  const h = setup(
    [{ id: "self", rate: 2 }],
    { latency: true, download: true, upload: true },
    { downloadMs: 1_000 },
  );
  h.plan.duration.downloadMs = 0;
  expect(() => h.start()).toThrow("Give at least one stage a duration");
  h.plan.duration.downloadMs = 1_000;
  h.start();
  const result = await h.result();
  expect(h.phases()).toEqual(["connecting", "download"]);
  expect(result.stages).toEqual({
    latency: "not-run",
    download: "complete",
    upload: "not-run",
    bidirectional: "not-run",
  });
});

test("a grant refused while the next stage prepares asks for sign-in", async () => {
  const server = { id: "a", name: "a", url: "https://a.example" };
  const h = setup(
    two({
      prepare: ({ stage }) => {
        if (stage === "upload")
          throw new Error("upload session could not be established", {
            cause: new SignInRequired(server),
          });
      },
    }),
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  const result = await h.result();
  expect(result.multiServer.failures).toMatchObject([
    { serverId: "a", stage: "upload", reason: "sign-in-required", message: "Sign in to a" },
  ]);
});

test("a lane that fails while a measured stage shuts down never fails the stage, however late", async () => {
  const torn = (delayMs: number): Partial<Peer> => ({
    checkpoint: () => new Promise((resolve) => later(delayMs, () => resolve(null))),
    finish: (events) => events.failed("connection-lost", "upload stream closed"),
  });
  for (const delayMs of [0, 1_200]) {
    const h = setup([{ id: "a", ...torn(delayMs) }], { upload: true }, { uploadMs: 1_000 });
    h.start();
    const result = await h.result();
    expect(result.stages.upload).toBe("complete");
    expect(result.multiServer.failures).toEqual([]);
    near(result.upload?.reportedBytesPerSec, 1_000);
  }
});

test("a final checkpoint answered late over a full uplink keeps a server that was moving", async () => {
  // The feed's last record comes at 900 ms and the closing checkpoint 1.2 s later, as if the server stood still two unmeasured seconds.
  const late: Partial<Peer> = {
    silent: (_activity, ms) => ms > 900,
    checkpoint: () =>
      new Promise((resolve) =>
        later(1_200, () =>
          resolve({ id: "a:upload", bytes: 900, nanos: 900e6 + 1 + 2e9, receivedAtMs: 0 }),
        ),
      ),
  };
  const h = setup([{ id: "a", ...late }], { upload: true }, { uploadMs: 1_000 });
  h.start();
  const result = await h.result();
  expect(result.multiServer.participants).toEqual(["a"]);
  expect(result.multiServer.failures).toEqual([]);
});

test("a stream that breaks as its grant ends leaves asking for sign-in, once the check says so", async () => {
  // The stream breaks 300 ms in and stays silent; the silence rule removes the server before any check could answer.
  const broken: Partial<Peer> = {
    silent: (activity, ms) => activity.stage === "download" && ms >= 300,
    measure: (events) =>
      later(300, () =>
        events.stalled({ reason: "connection-lost", detail: "stream broke", direction: "down" }),
      ),
  };
  for (const [signedOut, reason] of [
    [true, "sign-in-required"],
    [false, "connection-lost"],
  ] as const) {
    const h = setup(two({ ...broken, signedOut }), { download: true }, { downloadMs: 3_000 });
    h.start();
    const result = await h.result();
    expect(result.multiServer.failures).toMatchObject([
      { serverId: "a", stage: "download", reason },
    ]);
    expect(result.outcome).toBe("partial");
  }
});

test("an expired grant at the final checkpoint asks for sign-in and removes only that server", async () => {
  const server = { id: "a", name: "a", url: "https://a.example" };
  const h = setup(
    two({ checkpoint: () => Promise.reject(new SignInRequired(server)) }),
    { upload: true, download: true },
    { uploadMs: 1_000, downloadMs: 1_000 },
  );
  h.start();
  const result = await h.result();
  expect(result.multiServer.failures).toMatchObject([
    { serverId: "a", stage: "upload", reason: "sign-in-required", message: "Sign in to a" },
  ]);
  expect(result.multiServer.participants).toEqual(["b"]);
});

test("removing one server keeps the outcomes the others report in the same preparation or final checkpoint", async () => {
  const preparing = setup(
    two(
      { prepare: (_, events) => events.failed("sign-in-required", "fixture") },
      {
        prepare: () =>
          new Promise((_, reject) =>
            later(10, () => reject(new StageFailure("server-busy", "busy"))),
          ),
      },
    ),
    { download: true },
    { downloadMs: 1_000 },
  );
  preparing.start();
  await expect(preparing.result()).rejects.toMatchObject({ reason: "server-busy" });

  let b: StageEvents;
  const server = { id: "a", name: "a", url: "https://a.example" };
  const ending = setup(
    two(
      { checkpoint: () => Promise.reject(new SignInRequired(server)) },
      {
        measure: (events) => (b = events),
        checkpoint: () => {
          b.failed("protocol-error", "fixture");
          return Promise.resolve(null);
        },
      },
    ),
    { upload: true },
    { uploadMs: 1_000 },
  );
  ending.start();
  const result = await ending.result();
  expect(result.multiServer.failures.map(({ serverId, reason }) => [serverId, reason])).toEqual([
    ["b", "protocol-error"],
    ["a", "sign-in-required"],
  ]);
});

test("a live change while a stage ends measures no warmup and never runs a stage turned off", async () => {
  const slow = { finish: () => new Promise<void>((resolve) => later(300, resolve)) };
  for (const upload of [true, false]) {
    const h = setup(
      two(slow, slow),
      { download: true, upload: true, bidirectional: true },
      { warmupMs: 400, downloadMs: 1_000, uploadMs: 1_000, bidirectionalMs: 1_000 },
    );
    h.start();
    const ending = () => h.calls.filter((call) => call.startsWith("end:")).length === 2;
    for (let t = 0; t < 6_000 && !ending(); t += 5) await h.elapse(5);
    expect(h.run.canChange("upload")).toBe(true);
    expect(h.run.canChange("download")).toBe(false);
    h.run.reconfigure({
      stages: { ...h.plan.stages, upload },
      duration: h.plan.duration,
      adaptive: false,
    });
    const result = await h.result();
    expect(h.calls.filter((call) => call === "measure:a")).toHaveLength(upload ? 3 : 2);
    expect(result.stages.upload).toBe(upload ? "complete" : "not-run");
    expect(result.multiServer.intervals.map(({ stage, reason }) => `${stage}:${reason}`)).toEqual([
      "download:stage-start",
      ...(upload ? ["upload:stage-start"] : []),
      "bidirectional:stage-start",
    ]);
  }
});

test("a finished stage keeps the plan it ran with when a later change lengthens it", async () => {
  const h = setup(
    [{ id: "self" }],
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  await h.elapse(1_500);
  h.run.reconfigure({
    stages: h.plan.stages,
    duration: { ...h.plan.duration, downloadMs: 20_000 },
    adaptive: true,
  });
  const result = await h.result();
  expect(result.outcome).toBe("complete");
});

test("an unknown upload id replaces the receiver once, even while the server is already recovering", async () => {
  const replaced: AbortSignal[] = [];
  const measure = (events: StageEvents) =>
    later(300, () => {
      events.stalled({ reason: "connection-lost", detail: "", direction: "up" });
      for (let i = 0; i < 2; i++)
        events.stalled({ reason: "connection-lost", detail: "", direction: "up", rotate: true });
      events.resumed();
    });
  const h = setup(
    [{ id: "a", measure, replaceUpload: async (signal) => void replaced.push(signal) }],
    { upload: true },
    { uploadMs: 1_000 },
  );
  h.start();
  await h.result();
  expect(replaced).toHaveLength(1);
  expect(replaced[0]!.aborted).toBe(true);
});

test("a measurement checkpoint answered after a newer feed record never takes the count back", async () => {
  const h = setup(two({ checkpointLagMs: 300 }), { upload: true }, { uploadMs: 2_000 });
  h.start();
  const upload = (await h.result()).multiServer.intervals.filter(({ stage }) => stage === "upload");
  expect(upload).toHaveLength(1);
  expect(upload[0]!.complete).toBe(true);
});

test("each server ends its stage as soon as its own final evidence arrives", async () => {
  let slow = false;
  const h = setup(
    two({
      checkpoint: () =>
        slow
          ? new Promise((resolve) =>
              later(1_000, () => resolve({ id: "a", bytes: 0, nanos: 1, receivedAtMs: 0 })),
            )
          : undefined,
      measure: () => later(1_050, () => (slow = true)),
    }),
    { upload: true },
    { uploadMs: 1_100 },
  );
  h.start();
  await h.elapse(1_400);
  expect(h.calls.filter((call) => call.startsWith("end:"))).toEqual(["end:b"]);
  await h.result();
});

test("an aborted stage end delivers no late events", async () => {
  let release!: () => void;
  const hold = { finish: () => new Promise<void>((resolve) => (release = resolve)) };
  const h = setup(
    two(hold, hold),
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  await h.elapse(1_100);
  expect(h.calls).toContain("end:a");
  h.run.abort();
  const count = h.events.length;
  expect(h.events.at(-1)).toMatchObject({ type: "aborted" });
  release();
  await h.elapse(500);
  expect(h.events).toHaveLength(count);
});

test("no progress runs ahead of the segment shown, and a finalizing stage stops its clock", async () => {
  const slow = { finish: () => new Promise<void>((resolve) => later(800, resolve)) };
  const h = setup(
    [{ id: "self", ...slow }],
    { download: true, upload: true },
    { warmupMs: 300, downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  await h.result();
  let step = "";
  for (const event of h.events) {
    if (event.type === "phase") step = event.step;
    if (event.type === "progress" && step !== "running")
      expect({ step, progress: event }).toMatchObject({
        step: "finalizing",
        progress: { elapsedMs: 1_000 },
      });
  }
});

test("evidence that stops below the silence limit ends with its stage unless a failure is still unrecovered there", async () => {
  const silent = (activity: Activity, ms: number) => activity.stage === "download" && ms >= 400;
  const stall = (events: StageEvents, { stage }: Activity) => {
    if (stage === "download")
      later(400, () => events.stalled({ reason: "connection-lost", detail: "quiet" }));
  };
  const quiet = setup(
    two({ silent }),
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  quiet.start();
  const kept = await quiet.result();
  expect(kept.multiServer.failures).toEqual([]);
  expect(kept.multiServer.participants).toEqual(["a", "b"]);

  const h = setup(
    two({ silent, measure: stall }),
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  const result = await h.result();
  expect(result.multiServer.failures[0]).toMatchObject({
    serverId: "a",
    stage: "download",
    reason: "connection-lost",
    message: "quiet",
  });
  expect(result.multiServer.participants).toEqual(["b"]);
  expect(h.calls.filter((call) => call === "measure:a")).toHaveLength(1);
  expect(h.events.some(({ type }) => type === "stall")).toBe(false);
  const [download] = result.multiServer.intervals;
  expect(download!.endMs - download!.startMs).toBeLessThan(500);

  // Evidence silent past the limit leaves the interval within the stage, reported or not.
  const early = setup(two({ silent }), { download: true }, { downloadMs: 3_500 });
  early.start();
  const dropped = await early.result();
  expect(dropped.multiServer.failures).toMatchObject([
    { serverId: "a", reason: "timeout", message: "down direction carried no data" },
  ]);
  const [, survivors] = dropped.multiServer.intervals;
  expect(survivors).toMatchObject({ reason: "dropout", participants: ["b"] });
  expect(survivors!.startMs).toBeLessThan(2_300);
  near(dropped.download?.reportedBytesPerSec, 3_000);

  const sole = setup(
    [{ id: "self", silent, measure: stall }],
    { download: true },
    { downloadMs: 2_000 },
  );
  sole.start();
  await sole.elapse(1_000);
  const stalls = sole.events.flatMap((event) => (event.type === "stall" ? [event.stall] : []));
  expect(stalls).toEqual([{ reason: "connection-lost", detail: "quiet" }]);
  await sole.result();
});

test("a stall report while evidence still flows keeps the server at its stage end", async () => {
  const stall = (events: StageEvents) =>
    later(900, () =>
      events.stalled({ reason: "connection-lost", detail: "feed", direction: "up" }),
    );
  const h = setup(two({ measure: stall }), { upload: true }, { uploadMs: 1_000 });
  h.start();
  const result = await h.result();
  expect(result.outcome).toBe("complete");
  expect(result.multiServer.participants).toEqual(["a", "b"]);
});

test("a quiet link keeps its stage to the planned end, and its silence counts in the result", async () => {
  const h = setup(
    [{ id: "self", silent: hole("download", 1_000, 3_000) }],
    { download: true },
    { downloadMs: 4_000 },
  );
  h.start();
  const result = await h.result();
  expect(result.durationMs).toBeGreaterThanOrEqual(4_000);
  expect(result.stages.download).toBe("complete");
  expect(result.multiServer.failures).toEqual([]);
  // Two of the four seconds carried nothing: the headline averages them in and the result says how long.
  near(result.download?.reportedBytesPerSec, 500);
  expect(result.download!.quietMs).toBeGreaterThan(1_800);
  expect(result.download!.quietMs).toBeLessThanOrEqual(2_000);
  expect(h.events.flatMap((event) => (event.type === "stall" ? [event.stall] : []))).toEqual([
    { reason: "timeout", detail: "" },
    null,
  ]);
  const quiet = h.events.flatMap((event) =>
    event.type === "live" && event.sample.quietMs !== null ? [event.sample] : [],
  );
  expect(quiet.every((sample) => sample.stalled && sample.down === 0)).toBe(true);
  expect(Math.max(...quiet.map((sample) => sample.quietMs!))).toBeGreaterThan(1_800);
});

test("silence the servers share removes none; silence one has alone while another moves removes it", async () => {
  const quiet = hole("download", 1_000, 3_000);
  const shared = setup(
    two({ silent: quiet }, { silent: quiet }),
    { download: true },
    { downloadMs: 4_000 },
  );
  shared.start();
  const kept = await shared.result();
  expect(kept.multiServer.failures).toEqual([]);
  expect(kept.multiServer.participants).toEqual(["a", "b"]);
  near(kept.download?.reportedBytesPerSec, 2_000);

  const own = setup(two({ silent: quiet }), { download: true }, { downloadMs: 4_000 });
  own.start();
  const dropped = await own.result();
  expect(dropped.multiServer.failures).toMatchObject([{ serverId: "a", reason: "timeout" }]);
  expect(dropped.multiServer.participants).toEqual(["b"]);
  near(dropped.download?.reportedBytesPerSec, 3_000);
});

test("a receiver feed that lags keeps its server, whose bytes arrive with its next record", async () => {
  // a's receiver sends no record for 2 s while b moves, then reports every byte it received meanwhile.
  const h = setup(
    two({ silent: hole("upload", 1_000, 3_000) }),
    { upload: true },
    { uploadMs: 4_000 },
  );
  h.start();
  const result = await h.result();
  expect(result.multiServer.failures).toEqual([]);
  expect(result.multiServer.participants).toEqual(["a", "b"]);
  near(result.upload?.reportedBytesPerSec, 4_000);
  expect(result.upload?.quietMs).toBe(0);
});

test("a stage with a hole never finishes early, however steady it runs after", async () => {
  const h = setup(
    [{ id: "self", silent: hole("download", 500, 1_200) }],
    { download: true },
    { downloadMs: 12_000 },
    { adaptive: true },
  );
  h.start();
  expect((await h.result()).durationMs).toBeGreaterThanOrEqual(12_000);
});

test("a result spanning too little of its stage settles Partial, as History judges it", async () => {
  // The receiver's first record arrives 3 s into a 4 s upload, so its result spans one second.
  const h = setup(
    [{ id: "self", silent: hole("upload", 0, 3_000) }],
    { upload: true },
    { uploadMs: 4_000 },
  );
  h.start();
  const result = await h.result();
  near(result.upload?.reportedBytesPerSec, 1_000);
  expect(result.stages.upload).toBe("partial");
  expect(result.multiServer.failures).toMatchObject([
    { serverId: "self", stage: "upload", reason: "insufficient-evidence" },
  ]);
});

test("one long page suspension still enters every segment in order", async () => {
  const h = setup(
    [{ id: "self" }],
    { download: true, upload: true },
    { warmupMs: 200, downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  await h.elapse(20);
  h.clock.t += 5_000;
  const result = await h.result();
  expect(h.phases()).toEqual(["connecting", "warmup", "download", "warmup", "upload"]);
  expect(result.outcome).toBe("complete");
});

test("a bidirectional stage whose download never moves fails however long upload flows", async () => {
  for (const [bidirectionalMs, reason] of [
    [1_000, "insufficient-evidence"],
    [4_000, "timeout"],
  ] as const) {
    const h = setup(
      [{ id: "self", silent: (_, __, dir) => dir === "down" }],
      { bidirectional: true },
      { bidirectionalMs },
    );
    h.start();
    const result = await h.result();
    expect(result.stages.bidirectional).toBe("failed");
    expect(result.bidirectional?.down).toBeNull();
    expect(result.multiServer.failures).toMatchObject([{ stage: "bidirectional", reason }]);
  }
});

test("a grant that lapses as a measured stage shuts down never fails the stage", async () => {
  const h = setup(
    [{ id: "self", finish: (events) => events.signInRequired("throughput") }],
    { download: true },
    {
      downloadMs: 1_000,
    },
  );
  h.start();
  const result = await h.result();
  expect(result.stages.download).toBe("complete");
  expect(result.multiServer.failures).toEqual([]);
});

test("ending while a stage prepares ignores what tearing it down reports", async () => {
  let reject = (_: Error) => {};
  const h = setup(
    two({
      prepare: () => new Promise<void>((_, fail) => (reject = fail)),
      finish: (events) => events.signInRequired("throughput"),
      discard: (events) => events.failed("connection-lost", "torn down"),
    }),
    { download: true },
    { downloadMs: 1_000 },
  );
  h.start();
  await h.elapse(100);
  h.run.end("sign-in-required", "Signed out");
  reject(new Error("torn down"));
  const result = await h.result();
  expect(
    result.multiServer.failures.map(({ serverId, message }) => `${serverId}:${message}`),
  ).toEqual(["a:Signed out", "b:Signed out"]);
  expect(result.multiServer.participants).toEqual(["a", "b"]);
});

test("a stage turned on while the last one ends still runs", async () => {
  const slow = { finish: () => new Promise<void>((resolve) => later(300, resolve)) };
  const h = setup(two(slow, slow), { download: true }, { downloadMs: 1_000, uploadMs: 1_000 });
  h.start();
  for (let t = 0; t < 3_000 && !h.calls.includes("end:b"); t += 5) await h.elapse(5);
  expect(h.run.canChange("upload")).toBe(true);
  h.run.reconfigure({
    stages: { ...h.plan.stages, upload: true },
    duration: h.plan.duration,
    adaptive: false,
  });
  const result = await h.result();
  expect(result.stages).toMatchObject({ download: "complete", upload: "complete" });
});

test("a change while a stage ends cannot reach the window that closed", async () => {
  const slow = { finish: () => new Promise<void>((resolve) => later(300, resolve)) };
  const h = setup(
    [{ id: "self", ...slow }],
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  for (let t = 0; t < 3_000 && !h.calls.includes("end:self"); t += 5) await h.elapse(5);
  h.run.reconfigure({
    stages: h.plan.stages,
    duration: { ...h.plan.duration, downloadMs: 20_000 },
    adaptive: false,
  });
  const result = await h.result();
  expect(result.stages.download).toBe("complete");
});

test("a focus server lost after the latency stage hands every latency figure to a survivor", async () => {
  const h = setup(
    [
      { id: "b", measure: probe(2, 4) },
      {
        id: "c",
        measure: (events, { stage }) =>
          stage === "latency" ? probe(40, 4)(events) : fail(10)(events),
      },
    ],
    { latency: true, download: true },
    { latencyMs: 50, downloadMs: 1_000 },
    { focus: "c" },
  );
  h.start();
  const result = await h.result();
  const b = result.multiServer.servers.find(({ server }) => server.id === "b")!;
  expect(result.multiServer.latencyFocus).toBe("b");
  expect(result.latency?.reportedMs).toBe(2);
  expect(result.latencyByStage).toEqual(b.latencyByStage);
  expect(result.stages.latency).toBe("complete");
});

test("a server still retrying a failure it did not describe names the direction it left", async () => {
  const silent = (activity: Activity, ms: number) => activity.stage === "download" && ms >= 400;
  const stall = (events: StageEvents, { stage }: Activity) => {
    if (stage === "download")
      later(400, () => events.stalled({ reason: "connection-lost", detail: "" }));
  };
  const h = setup(two({ silent, measure: stall }), { download: true }, { downloadMs: 1_000 });
  h.start();
  const result = await h.result();
  // b stayed, silent too, and says why it has no result of its own.
  expect(result.multiServer.failures).toMatchObject([
    { serverId: "a", message: "down direction carried no data" },
    { serverId: "b", reason: "insufficient-evidence" },
  ]);
  const b = result.multiServer.servers.find(({ server }) => server.id === "b")!;
  expect(b.stages.download).toBe("failed");
});

test("a steady upload and a steady latency stage each finish early", async () => {
  const upload = setup([{ id: "self" }], { upload: true }, { uploadMs: 6_000 }, { adaptive: true });
  upload.start();
  expect((await upload.result()).durationMs).toBeLessThan(6_000);

  const steady = (events: StageEvents) => void clock.every(100, () => probe(10)(events));
  const latency = setup(
    [{ id: "self", measure: steady }],
    { latency: true },
    { latencyMs: 20_000 },
    { adaptive: true },
  );
  latency.start();
  const result = await latency.result(40_000);
  expect(result.durationMs).toBeLessThan(20_000);
  expect(result.latency?.reportedMs).toBe(10);
});

test("an early finish's steady window opens on what the lanes had counted there, however late they report", async () => {
  // 1 byte per ms, each reported 30 ms after it was counted: a tick sees 30 bytes too few.
  let started = 0;
  let reported = 0;
  const report = (events: StageEvents, lagMs: number) => {
    const counted = Math.max(0, clock.t - started - lagMs);
    events.download(counted - reported);
    reported = counted;
  };
  const h = setup(
    [
      {
        id: "self",
        rate: 0,
        measure: (events) => {
          started = clock.t;
          clock.every(20, () => report(events, 30));
        },
        countAt: (at) => at - started,
        flush: (events) => report(events, 0),
      },
    ],
    { download: true },
    { downloadMs: 6_000 },
    { adaptive: true },
  );
  h.start();
  const result = await h.result();
  expect(result.durationMs).toBeLessThan(6_000);
  expect(result.download?.reportedBytesPerSec).toBe(1_000);
});

test("turning adaptive off mid-stage runs the stage to its planned end", async () => {
  const h = setup([{ id: "self" }], { download: true }, { downloadMs: 6_000 }, { adaptive: true });
  h.start();
  await h.elapse(1_000);
  h.run.reconfigure({ stages: h.plan.stages, duration: h.plan.duration, adaptive: false });
  expect((await h.result()).durationMs).toBeGreaterThanOrEqual(6_000);
});

test("a page gap while measuring starts a new interval and forbids an early finish", async () => {
  const h = setup([{ id: "self" }], { download: true }, { downloadMs: 12_000 }, { adaptive: true });
  h.start();
  await h.elapse(1_000);
  h.clock.t += 3_000;
  const result = await h.result();
  expect(result.durationMs).toBeGreaterThanOrEqual(12_000);
  expect(result.multiServer.intervals.map(({ reason }) => reason)).toEqual([
    "stage-start",
    "evidence-resumed",
  ]);
});

test("a receiver record delivered before the first tick after a page gap opens the resumed interval", async () => {
  // The receiver counts 1 byte per ms of its own time, except while the page and its workers are frozen.
  let frozen = false;
  let emit = () => {};
  let bytes = 0;
  let nanos = 1;
  let last = 0;
  const record = () => {
    nanos += (clock.t - last) * 1e6;
    if (!frozen) bytes += clock.t - last;
    last = clock.t;
    return { id: "u", bytes, nanos, receivedAtMs: clock.t };
  };
  const h = setup(
    [
      {
        id: "self",
        receives: false,
        measure: (events) => {
          last = clock.t;
          emit = () => events.receiver(record());
          clock.every(100, emit);
        },
        checkpoint: () => Promise.resolve(record()),
      },
    ],
    { upload: true },
    { uploadMs: 10_000 },
  );
  h.start();
  await h.elapse(8_000);
  frozen = true;
  h.clock.t += 20_000;
  // The feed's buffered record lands before the run's next timer task.
  emit();
  frozen = false;
  const result = await h.result();
  near(result.upload?.reportedBytesPerSec, 1_000);
  expect(result.multiServer.intervals.map(({ reason }) => reason)).toEqual([
    "stage-start",
    "evidence-resumed",
  ]);
});

test("a server's result from an early stage outlives the interval limit a later stage reaches", async () => {
  const h = setup(
    [{ id: "self" }],
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 600_000 },
  );
  h.start();
  await h.elapse(1_500);
  // Each page gap opens an interval; past 128 the oldest, the download's, give way.
  for (let gap = 0; gap < 140; gap++) {
    h.clock.t += 2_000;
    await h.elapse(400);
  }
  const result = await h.result(600_000);
  expect(result.multiServer.omittedIntervals).toBeGreaterThan(0);
  near(result.multiServer.servers[0]!.download?.reportedBytesPerSec, 1_000);
});

test("a stage end handler may end or stop the run there, and one that throws changes nothing", async () => {
  const reported = spyOn(globalThis, "reportError").mockImplementation(() => {});
  try {
    for (const act of ["end", "abort", "throw"] as const) {
      const h = setup(
        [{ id: "self" }],
        { download: true, upload: true },
        { downloadMs: 1_000, uploadMs: 1_000 },
      );
      h.start();
      h.run.on((event) => {
        if (event.type !== "stageEnd" || event.stage !== "download") return;
        if (act === "end") h.run.end("sign-in-required", "Signed out");
        else if (act === "abort") h.run.abort();
        else throw new Error("handler");
      });
      await h.elapse(5_000);
      const ends = h.events.filter(
        ({ type }) => type === "complete" || type === "aborted" || type === "error",
      );
      expect(ends.map(({ type }) => type)).toEqual([act === "abort" ? "aborted" : "complete"]);
      expect(h.calls.includes("begin:self:upload")).toBe(act === "throw");
    }
    expect(reported).toHaveBeenCalledTimes(1);
  } finally {
    reported.mockRestore();
  }
});

test("ending or stopping from a summary handler during a stage end delivers nothing after the terminal event", async () => {
  const late: Record<string, string[]> = {};
  for (const act of ["end", "abort"] as const) {
    const h = setup(
      [{ id: "self" }],
      { download: true, upload: true },
      { downloadMs: 1_000, uploadMs: 1_000 },
    );
    h.start();
    let done = false;
    h.run.on((event) => {
      if (done || event.type !== "summary" || event.stage !== "download") return;
      done = true;
      if (act === "end") h.run.end("sign-in-required", "Signed out");
      else h.run.abort();
    });
    await h.elapse(5_000);
    const i = h.events.findIndex(({ type }) => type === "complete" || type === "aborted");
    late[act] = h.events.slice(i + 1).map(({ type }) => type);
  }
  expect(late).toEqual({ end: [], abort: [] });
});

test("turning the next stage off from a stage end handler never runs it", async () => {
  const h = setup(
    [{ id: "self" }],
    { download: true, upload: true, bidirectional: true },
    { warmupMs: 300, downloadMs: 1_000, uploadMs: 1_000, bidirectionalMs: 1_000 },
  );
  h.start();
  h.run.on((event) => {
    if (event.type === "stageEnd" && event.stage === "download")
      h.run.reconfigure({
        stages: { ...h.plan.stages, upload: false },
        duration: h.plan.duration,
        adaptive: false,
      });
  });
  const result = await h.result();
  expect(h.calls).not.toContain("begin:self:upload");
  expect(result.stages.upload).toBe("not-run");
});

test("turning the next stage on from a stage end handler runs the stages in order", async () => {
  const h = setup(
    [{ id: "self" }],
    { download: true, bidirectional: true },
    { warmupMs: 300, downloadMs: 1_000, uploadMs: 1_000, bidirectionalMs: 1_000 },
  );
  h.start();
  h.run.on((event) => {
    if (event.type === "stageEnd" && event.stage === "download")
      h.run.reconfigure({
        stages: { ...h.plan.stages, upload: true },
        duration: h.plan.duration,
        adaptive: false,
      });
  });
  const result = await h.result();
  expect(h.calls.filter((call) => call.startsWith("begin:"))).toEqual([
    "begin:self:download",
    "begin:self:upload",
    "begin:self:bidirectional",
  ]);
  expect(result.stages).toMatchObject({
    download: "complete",
    upload: "complete",
    bidirectional: "complete",
  });
});

test("a focus that measured no latency and leaves later hands the headline and its status to a survivor", async () => {
  const h = setup(
    [
      {
        id: "b",
        measure: (events, { stage }) => void (stage === "latency" && probe(2, 4)(events)),
      },
      { id: "c", measure: (events, { stage }) => void (stage === "download" && fail(10)(events)) },
    ],
    { latency: true, download: true },
    { latencyMs: 50, downloadMs: 1_000 },
    { focus: "c" },
  );
  h.start();
  const result = await h.result();
  expect({
    status: result.stages.latency,
    latency: result.latency?.reportedMs ?? null,
    focus: result.multiServer.latencyFocus,
  }).toEqual({ status: "partial", latency: 2, focus: "b" });
});

test("a latency headline outlives every server that measured it leaving later", async () => {
  const h = setup(
    [
      {
        id: "b",
        measure: (events, { stage }) =>
          stage === "latency" ? probe(2, 4)(events) : fail(10)(events),
      },
      {
        id: "c",
        measure: (events, { stage }) =>
          void (stage === "latency" && (probe(40, 1)(events), fail(10)(events))),
      },
      { id: "e", latency: false },
    ],
    { latency: true, download: true },
    { latencyMs: 50, downloadMs: 1_000 },
    { focus: "c" },
  );
  h.start();
  const result = await h.result();
  const stageEnd = h.events.find((event) => event.type === "stageEnd" && event.stage === "latency");
  expect({
    status: result.stages.latency,
    atStageEnd: stageEnd?.type === "stageEnd" ? stageEnd.results.latency?.reportedMs : undefined,
    latency: result.latency?.reportedMs ?? null,
    focus: result.multiServer.latencyFocus,
  }).toEqual({ status: "partial", atStageEnd: 2, latency: 2, focus: "b" });
});

test("ending from the phase event of a preparing stage leaves no resources open", async () => {
  const h = setup(
    [{ id: "self" }],
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  h.run.on((event) => {
    if (event.type === "phase" && event.stage === "upload" && event.step === "preparing")
      h.run.end("sign-in-required", "Signed out");
  });
  await h.result();
  const upload = h.calls.slice(h.calls.indexOf("begin:self:upload"));
  expect(upload.some((call) => call === "end:self" || call === "discard:self")).toBe(true);
});

test("a failure handler that ends the run while preparation results are removed completes it", async () => {
  const refuse = { prepare: () => Promise.reject(new Error("fixture")) };
  const h = setup(two(refuse, refuse), { download: true }, { downloadMs: 1_000 });
  h.start();
  let once = false;
  h.run.on((event) => {
    if (event.type !== "failure" || once) return;
    once = true;
    h.run.end("sign-in-required", "Signed out");
  });
  const result = await h.result();
  expect(result.stages.download).toBe("failed");
});

test("a run that is finishing refuses to turn a stage on", async () => {
  const h = setup([{ id: "self" }], { download: true }, { downloadMs: 2_000 });
  h.start();
  await h.elapse(500);
  h.run.end("sign-in-required", "Signed out");
  expect(h.run.canChange("upload")).toBe(false);
  expect(() =>
    h.run.reconfigure({
      stages: { ...h.plan.stages, upload: true },
      duration: { ...h.plan.duration, uploadMs: 1_000 },
      adaptive: false,
    }),
  ).toThrow();
  await h.result();
});

test("stopping from the phase event of a preparing stage leaves no resources open", async () => {
  const h = setup(
    [{ id: "self" }],
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
  );
  h.start();
  h.run.on((event) => {
    if (event.type === "phase" && event.stage === "upload" && event.step === "preparing")
      h.run.abort();
  });
  await h.elapse(5_000);
  const begun = h.calls.indexOf("begin:self:upload");
  expect(
    begun < 0 ||
      h.calls.slice(begun).some((call) => call === "end:self" || call === "discard:self"),
  ).toBe(true);
});

test("a stage that cannot prepare reports the reason its lanes gave", async () => {
  const h = setup(
    [{ id: "self", prepare: () => Promise.reject(new StageFailure("protocol-error", "HTTP 400")) }],
    { download: true },
    { downloadMs: 1_000 },
  );
  h.start();
  await expect(h.result()).rejects.toMatchObject({
    reason: "protocol-error",
    message: expect.stringContaining("HTTP 400"),
  });
});

test("a stage's readiness waits and checkpoints end as it settles", async () => {
  const signals: AbortSignal[] = [];
  const h = setup(
    [{ id: "a", checkpoint: (signal) => void signals.push(signal) }],
    { upload: true, bidirectional: true },
    {
      uploadMs: 1_000,
      bidirectionalMs: 1_000,
    },
  );
  h.start();
  let ended: boolean[] = [];
  h.run.on((event) => {
    if (event.type === "stageEnd" && event.stage === "upload")
      ended = signals.map(({ aborted }) => aborted);
  });
  await h.result();
  expect(ended.length).toBeGreaterThan(1);
  expect(ended.every(Boolean)).toBe(true);
});

test("lanes a later stage cannot be dealt end the run with what earlier stages measured", async () => {
  let dealt = 0;
  const h = setup(
    [{ id: "a" }],
    { download: true, upload: true },
    { downloadMs: 1_000, uploadMs: 1_000 },
    {
      deal: (activity) => {
        if (activity.stage === "upload" && dealt++) throw new Error("No lanes for this stage");
      },
    },
  );
  h.start();
  const result = await h.result();
  expect(result.download?.reportedBytesPerSec).toBeGreaterThan(0);
  expect([result.stages.upload, result.outcome]).toEqual(["failed", "incomplete"]);
  expect(result.multiServer.failures).toMatchObject([
    { stage: "upload", message: "No lanes for this stage" },
  ]);
});
