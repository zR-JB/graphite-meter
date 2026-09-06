import { test, expect, beforeEach, afterEach, spyOn } from "bun:test";
import {
  RunnerCore,
  STAGE_RECOVERY_BUDGET_MS,
  type RunnerBackend,
  type CoreHost,
  type RunMeasurementSource,
} from "./core";
import type {
  RunnerConfig,
  RunnerEvent,
  PhaseActivity,
  Phase,
} from "./contract";
import { latencyPresentationBucketMs } from "./latencyBuckets";
import { fixedPingIntervalMs } from "./pingCadence";
import { RunAccumulator } from "./evaluation";
import { DEFAULT_CONFIG } from "../state/defaults";
import { stubGlobals } from "../test-helpers.test";
import { TEST_BUILD_TOKENS, testPreparedPaths } from "./test-helpers.test";
let fakeNow = 0;
let tickCallback: (() => void) | null = null;
const realNow = performance.now.bind(performance);
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;
beforeEach(() => {
  fakeNow = 0;
  tickCallback = null;
  performance.now = () => fakeNow;
  globalThis.setTimeout = ((fn: () => void) => {
    tickCallback = fn;
    return 1 as unknown as ReturnType<typeof setTimeout>;
  }) as typeof setTimeout;
  globalThis.clearTimeout = (() => {
    tickCallback = null;
  }) as typeof clearTimeout;
});
afterEach(() => {
  performance.now = realNow;
  globalThis.setTimeout = realSetTimeout;
  globalThis.clearTimeout = realClearTimeout;
});
function advance(ms: number): void {
  fakeNow += ms;
  tickCallback?.();
}
type BackendOptions = {
  deferred?: boolean;
  flush?: "sync" | "async";
};
class FakeBackend implements RunnerBackend {
  constructor(private readonly options: BackendOptions = {}) {}
  host!: CoreHost;
  prepared?: () => void;
  flush?: () => void;
  calls: string[] = [];
  recoveries: RecoveryRequest[] = [];
  attach(host: CoreHost): void {
    this.host = host;
  }
  onRunStart(): void {
    this.calls.push("runStart");
  }
  onStageBegin(activity: PhaseActivity): void | Promise<void> {
    this.calls.push(`begin:${activity.stage}`);
    if (this.options.deferred)
      return new Promise((resolve) => (this.prepared = resolve));
  }
  onStageMeasure(activity: PhaseActivity): void {
    this.calls.push(`measure:${activity.stage}`);
  }
  onStageEnd(activity: PhaseActivity): void | Promise<void> {
    this.calls.push(`end:${activity.stage}`);
    if (this.options.flush === "sync" && activity.stage === "download")
      this.host.ingestThroughput("down", 100, 0.1);
    if (this.options.flush !== "async") return;
    return new Promise(
      (resolve) =>
        (this.flush = () => {
          this.host.ingestThroughput("down", 100, 0.1);
          resolve();
        }),
    );
  }
  onStageRecovery(request: RecoveryRequest): void {
    this.recoveries.push(request);
  }
  onComplete(): void {
    this.calls.push("complete");
  }
  onAbort(): void {
    this.calls.push("abort");
  }
  setBackgroundActivity(enabled: boolean): void {
    this.calls.push(`background:${enabled}`);
  }
}
type RecoveryRequest = NonNullable<
  Parameters<NonNullable<RunnerBackend["onStageRecovery"]>>[0]
>;
type ConfigOverrides = {
  stages?: Partial<RunnerConfig["stages"]>;
  duration?: Partial<RunnerConfig["duration"]>;
  adaptive?: Partial<RunnerConfig["adaptive"]>;
  pingCadence?: RunnerConfig["pingCadence"];
  loadedPingCadence?: RunnerConfig["loadedPingCadence"];
};
const stageDefaults: RunnerConfig["stages"] = {
  latency: false,
  download: true,
  upload: false,
  bidirectional: false,
};
const durationDefaults: RunnerConfig["duration"] = {
  warmupMs: 0,
  latencyMs: 0,
  downloadMs: 1000,
  uploadMs: 0,
  bidirectionalMs: 0,
};
const adaptiveDefaults: RunnerConfig["adaptive"] = {
  enabled: false,
  minCoverageRatio: 0,
  stabilityThreshold: 0.9,
  maxPhaseReductionRatio: 1,
  minLatencySamples: 0,
  minTransferSamples: 0,
  confirmationMs: 100,
};
function makeConfig(overrides: ConfigOverrides = {}): RunnerConfig {
  const base = structuredClone(DEFAULT_CONFIG);
  return {
    ...base,
    stages: { ...stageDefaults, ...overrides.stages },
    duration: { ...durationDefaults, ...overrides.duration },
    pingCadence: overrides.pingCadence ?? base.pingCadence,
    loadedPingCadence: overrides.loadedPingCadence ?? base.loadedPingCadence,
    transferStreams: { mode: "auto", count: 6 },
    transports: {
      throughputTarget: "current",
      latencyTarget: "auto",
    },
    adaptive: { ...adaptiveDefaults, ...overrides.adaptive },
  };
}
function phaseTransitions(events: RunnerEvent[]): Phase[] {
  return typedEvents(events, "phase").map((event) => event.transition.to);
}
function typedEvents<T extends RunnerEvent["type"]>(
  events: RunnerEvent[],
  type: T,
): Extract<RunnerEvent, { type: T }>[] {
  return events.filter(
    (event): event is Extract<RunnerEvent, { type: T }> => event.type === type,
  );
}
const progressEvents = (events: RunnerEvent[]) =>
  typedEvents(events, "progress");
type SampleEvent<T extends "latency" | "throughput"> = Extract<
  RunnerEvent,
  { type: T }
>;
function eventSamples(
  events: RunnerEvent[],
  type: "latency",
): SampleEvent<"latency">["sample"][];
function eventSamples(
  events: RunnerEvent[],
  type: "throughput",
): SampleEvent<"throughput">["sample"][];
function eventSamples(events: RunnerEvent[], type: "latency" | "throughput") {
  return typedEvents(events, type).flatMap((event) =>
    "sample" in event ? [event.sample] : [],
  );
}
const completeEvent = (events: RunnerEvent[]) =>
  typedEvents(events, "complete")[0];
function hasEvent(events: RunnerEvent[], type: RunnerEvent["type"]): boolean {
  return events.some((event) => event.type === type);
}
function expectComplete(
  events: RunnerEvent[],
  check: (result: Extract<RunnerEvent, { type: "complete" }>["result"]) => void,
): void {
  const event = completeEvent(events);
  expect(event).toBeDefined();
  if (event?.type === "complete") check(event.result);
}
type CoreRun = {
  backend: FakeBackend;
  core: RunnerCore;
  events: RunnerEvent[];
};
function observeCore(backend = new FakeBackend()): CoreRun {
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((event) => events.push(event));
  return { backend, core, events };
}

test("a coordinated source owns latency populations without a second mixed presentation stream", () => {
  const accumulator = new RunAccumulator();
  const source: RunMeasurementSource = {
    confidence: (stage) => accumulator.confidence(stage),
    trackStableRun: () => false,
    canComplete: () => true,
    armLatencyEarlyStop: () => accumulator.armLatencyEarlyStop(),
    cancelLatencyEarlyStop: () => accumulator.cancelLatencyEarlyStop(),
    confirmLatencyEarlyStop: () => accumulator.confirmLatencyEarlyStop(),
    throughputResult: () => null,
    bidirectionalResult: () => ({ down: null, up: null }),
    latencyResult: () => null,
    latencySummaries: () => ({
      latency: null,
      download: null,
      upload: null,
      bidirectional: null,
    }),
    bufferbloatGrade: () => null,
    details: () => ({
      selection: [],
      participants: [],
      latencyFocus: "",
      servers: [],
      intervals: [],
      omittedIntervals: 0,
      failures: [],
    }),
  };
  const core = new RunnerCore(new FakeBackend(), source);
  const events: RunnerEvent[] = [];
  core.on((event) => events.push(event));
  core.start(
    makeConfig({
      stages: { latency: true, download: false },
      duration: { latencyMs: 1000 },
      adaptive: { enabled: false },
    }),
    0,
  );
  advance(1);
  for (let i = 0; i < 900; i++) {
    core.ingestLatency({ observedAtMs: fakeNow, rttMs: i % 4, lost: false });
    advance(1);
  }
  expect(events.filter((event) => event.type === "latency")).toEqual([]);
  advance(1000);
  expect(events.some((event) => event.type === "complete")).toBe(true);
  core.dispose();
});

test.each([
  ["without extending duration", false],
  ["after extending duration", true],
] as const)(
  "coordinated latency retains each confirmed population %s",
  async (_description, extendDuration) => {
    const restore = stubGlobals(TEST_BUILD_TOKENS);
    const { ServerCoordinator } = await import("../servers/coordinator");
    const hosts: CoreHost[] = [];
    const coordinator = new ServerCoordinator(
      ["a", "b"].map((id) => ({
        server: { id, name: id, url: `https://${id}.example` },
        paths: testPreparedPaths(),
      })),
      "a",
      () => ({
        attach(host) {
          hosts.push(host);
        },
        onRunStart() {},
        onStageBegin() {},
        onStageMeasure() {},
        onStageEnd() {},
        onAbort() {},
        onComplete() {},
        flushDownload() {},
        checkpoint: async () => null,
      }),
    );
    const events: RunnerEvent[] = [];
    coordinator.on((event) => events.push(event));
    const cfg = makeConfig({
      stages: { latency: true, download: false },
      duration: { latencyMs: 4000 },
      adaptive: {
        enabled: true,
        minCoverageRatio: 0.5,
        minLatencySamples: 3,
        confirmationMs: 500,
      },
    });
    const settleStageWork = () =>
      new Promise<void>((resolve) => realSetTimeout(resolve, 0));
    try {
      coordinator.start(cfg, 0);
      advance(10);
      await settleStageWork();
      for (let i = 0; i < 55 && coordinator.phase !== "complete"; i++) {
        const rtt = fakeNow < 2000 ? 10 : fakeNow < 4000 ? 11 : 12;
        for (const [index, host] of hosts.entries())
          host.ingestLatency({
            rttMs: rtt + index * 20,
            lost: false,
            observedAtMs: fakeNow,
          });
        if (extendDuration && fakeNow === 2110) {
          cfg.duration.latencyMs = 8000;
          coordinator.reconfigure(cfg);
        }
        advance(100);
        await settleStageWork();
      }
      expectComplete(events, (result) => {
        const expected = extendDuration ? 12 : 11;
        expect(result.latency?.reportedMs).toBe(expected);
        expect(result.durationMs).toBeLessThan(cfg.duration.latencyMs);
        expect(
          result.multiServer?.servers.map(
            (server) => server.latency?.reportedMs,
          ),
        ).toEqual([expected, expected + 20]);
        expect(result.latencyByStage.latency?.p50Ms).toBeLessThan(expected);
        expect(
          result.multiServer?.servers[1].latencyByStage.latency?.p50Ms,
        ).toBeLessThan(expected + 20);
      });
    } finally {
      coordinator.dispose();
      restore();
    }
  },
);

type StartedCore = CoreRun & { cfg: RunnerConfig };
async function startCore(
  overrides: ConfigOverrides = {},
  backend = new FakeBackend(),
): Promise<StartedCore> {
  const run = observeCore(backend),
    cfg = makeConfig(overrides);
  run.core.start(cfg, 0);
  return { ...run, cfg };
}
async function startDownload(
  durationMs: number,
  overrides: ConfigOverrides = {},
): Promise<StartedCore> {
  return startCore({
    ...overrides,
    duration: { ...overrides.duration, downloadMs: durationMs },
  });
}
async function startStableDownload(
  durationMs: number,
  adaptive: Partial<RunnerConfig["adaptive"]> = {},
): Promise<StartedCore> {
  return startDownload(durationMs, {
    adaptive: {
      ...adaptiveDefaults,
      enabled: true,
      minTransferSamples: 4,
      ...adaptive,
    },
  });
}
function feedFlatThroughput(
  core: RunnerCore,
  count: number,
  bytes = 100,
  seconds = 0.1,
): void {
  for (let i = 0; i < count; i++) core.ingestThroughput("down", bytes, seconds);
}
function throughputContinuityIds(events: RunnerEvent[]): number[] {
  return events.flatMap((event) =>
    event.type === "throughput" ? [event.sample.continuityId] : [],
  );
}
function reconfigureDownload(
  core: RunnerCore,
  cfg: RunnerConfig,
  changes: {
    duration?: Partial<RunnerConfig["duration"]>;
    adaptive?: Partial<RunnerConfig["adaptive"]>;
  },
): void {
  core.reconfigure({
    stages: cfg.stages,
    duration: { ...cfg.duration, ...changes.duration },
    adaptive: { ...cfg.adaptive, ...changes.adaptive },
  });
}

test("full run: latency then download — phase order and stage lifecycle", async () => {
  const { backend, core, events } = await startCore({
    stages: { latency: true, download: true },
    duration: { latencyMs: 100, downloadMs: 100 },
  });
  expect(backend.calls).toEqual([
    "runStart",
    "begin:latency",
    "measure:latency",
  ]);
  expect(phaseTransitions(events)).toEqual(["connecting", "latency"]);
  core.ingestLatency({ rttMs: 12, lost: false, observedAtMs: fakeNow });
  advance(100);
  expect(backend.calls).toEqual([
    "runStart",
    "begin:latency",
    "measure:latency",
    "end:latency",
    "begin:download",
    "measure:download",
  ]);
  expect(phaseTransitions(events)).toEqual([
    "connecting",
    "latency",
    "download",
  ]);
  expect(core.phase).toBe("download");
  advance(100);
  expect(backend.calls).toEqual([
    "runStart",
    "begin:latency",
    "measure:latency",
    "end:latency",
    "begin:download",
    "measure:download",
    "end:download",
    "complete",
  ]);
  expect(core.phase).toBe("complete");
  expectComplete(events, (result) => {
    expect(result.download).not.toBeNull();
    expect(result.latency).not.toBeNull();
  });
});

test("failed-stage shutdown reports discarded probe accounting before the partial result", async () => {
  class InterruptedBackend extends FakeBackend {
    override onStageEnd(
      activity: PhaseActivity,
      flush = true,
    ): void | Promise<void> {
      if (!flush) {
        this.host.ingestLatencyAccountingIncomplete();
        return;
      }
      return super.onStageEnd(activity);
    }
  }
  const { core, backend, events } = await startCore(
    {
      stages: { download: true, upload: true },
      duration: { downloadMs: 1_000, uploadMs: 100 },
    },
    new InterruptedBackend(),
  );
  core.ingestThroughput("down", 900, 0.9);
  core.ingestLatency({ rttMs: 12, lost: false, observedAtMs: fakeNow });
  core.failStage(
    "download",
    "connection-lost",
    "transfer failed with pending probes",
  );
  const summaryBeforeResult = events.slice(
    0,
    events.findIndex(
      (event) => event.type === "stageResult" && event.stage === "download",
    ),
  );
  expect(
    typedEvents(summaryBeforeResult, "latencySummary").at(-1)?.summary,
  ).toMatchObject({
    accountingComplete: false,
    probeCount: 1,
    timeoutCount: 0,
    meanMs: 12,
  });
  expect(
    typedEvents(events, "stageResult").find(
      (event) => event.stage === "download",
    )?.result,
  ).toMatchObject({ totalBytes: 900 });
  expect(hasEvent(events, "stageSkipped")).toBe(true);
  advance(0);
  expect(backend.calls).toContain("begin:upload");
  advance(100);
  expectComplete(events, (result) => {
    expect(result.download?.totalBytes).toBe(900);
    expect(result.latencyByStage.download).toMatchObject({
      accountingComplete: false,
      probeCount: 1,
      timeoutCount: 0,
      meanMs: 12,
    });
  });
});

test("a terminal runner error retains previously reduced bidirectional lanes", async () => {
  const { core, events } = await startCore({
    stages: { download: false, bidirectional: true },
    duration: { bidirectionalMs: 1_000 },
  });
  core.ingestThroughput("down", 800, 0.8);
  core.ingestThroughput("up", 799, 0.799);
  core.failStage("bidirectional", "connection-lost", "downstream lost", "down");
  core.fail("internal-error", "later terminal error");
  const error = typedEvents(events, "error")[0];
  expect(error?.error.partial?.bidirectional?.down?.totalBytes).toBe(800);
  expect(error?.error.partial?.bidirectional?.up).toBeNull();
});

test("throughput stays isolated across transfer warmups", async () => {
  const { core, events } = await startCore({
    stages: { download: true, upload: true, bidirectional: true },
    duration: {
      warmupMs: 100,
      downloadMs: 100,
      uploadMs: 100,
      bidirectionalMs: 100,
    },
  });
  advance(100);
  core.ingestThroughput("down", 2_000_000, 0.1);
  advance(100);
  expect(core.phase).toBe("warmup");
  core.ingestThroughput("down", 2_000_000, 0.1);
  advance(100);
  core.ingestThroughput("up", 1_000_000, 0.1);
  advance(100);
  expect(core.phase).toBe("warmup");
  core.ingestThroughput("up", 1_000_000, 0.1);
  advance(100);
  core.ingestThroughput("down", 700_000, 0.1);
  core.ingestThroughput("up", 300_000, 0.1);
  const samples = eventSamples(events, "throughput");
  expect(samples).toHaveLength(4);
  expect(
    samples.map(({ phase, dir, bytesPerSec }) => ({
      phase,
      dir,
      bytesPerSec,
    })),
  ).toEqual([
    { phase: "download", dir: "down", bytesPerSec: 20_000_000 },
    { phase: "upload", dir: "up", bytesPerSec: 10_000_000 },
    { phase: "bidirectional", dir: "down", bytesPerSec: 7_000_000 },
    { phase: "bidirectional", dir: "up", bytesPerSec: 3_000_000 },
  ]);
});

test("phase transitions report scheduled boundaries when a tick overshoots", async () => {
  const { events } = await startCore({
    stages: { download: true, upload: true },
    duration: { warmupMs: 100, downloadMs: 100, uploadMs: 100 },
  });
  advance(125);
  advance(100);
  const transitions = events.flatMap((event) =>
    event.type === "phase" ? [event.transition] : [],
  );
  expect(transitions.map(({ to, t }) => ({ to, t }))).toEqual([
    { to: "connecting", t: 0 },
    { to: "warmup", t: 0 },
    { to: "download", t: 100 },
    { to: "warmup", t: 200 },
  ]);
});

test("warmup->measure seam: same stage, no onStageEnd between begin and measure", async () => {
  const { backend, events } = await startCore({
    duration: { warmupMs: 50, downloadMs: 100 },
  });
  expect(backend.calls).toEqual(["runStart", "begin:download"]);
  expect(phaseTransitions(events)).toEqual(["connecting", "warmup"]);
  advance(50);
  expect(backend.calls).toEqual([
    "runStart",
    "begin:download",
    "measure:download",
  ]);
  expect(phaseTransitions(events)).toEqual([
    "connecting",
    "warmup",
    "download",
  ]);
  advance(100);
  expect(backend.calls).toEqual([
    "runStart",
    "begin:download",
    "measure:download",
    "end:download",
    "complete",
  ]);
});

test("aborting stage preparation prevents its late continuation from measuring", async () => {
  const backend = new FakeBackend({ deferred: true });
  const { core } = observeCore(backend);
  core.start(makeConfig(), 0);
  core.abort();
  backend.prepared!();
  await Promise.resolve();
  expect(core.phase).toBe("aborted");
  expect(backend.calls).toEqual(["runStart", "begin:download", "abort"]);
});

test("prepared latency adjusts warmup without moving connection ownership into the core", () => {
  const { core, backend } = observeCore();
  core.start(makeConfig({ duration: { warmupMs: 100, downloadMs: 100 } }), 200);
  expect(core.phase).toBe("warmup");
  expect(core.config?.duration.warmupMs).toBeGreaterThan(100);
  expect(backend.calls).toEqual(["runStart", "begin:download"]);
});

test("asynchronous stage preparation cannot consume the warmup budget", async () => {
  const backend = new FakeBackend({ deferred: true });
  const { core, events } = observeCore(backend);
  await core.start(
    makeConfig({ duration: { warmupMs: 100, downloadMs: 100 } }),
    0,
  );
  advance(1000);
  expect(core.phase).toBe("warmup");
  expect(progressEvents(events).at(-1)?.phaseElapsedMs).toBe(0);
  expect(backend.calls).toEqual(["runStart", "begin:download"]);
  backend.prepared!();
  await Promise.resolve();
  advance(99);
  expect(core.phase).toBe("warmup");
  advance(1);
  expect(core.phase).toBe("download");
  expect(backend.calls).toEqual([
    "runStart",
    "begin:download",
    "measure:download",
  ]);
});

test("asynchronous preparation starts the measured silence budget", async () => {
  const backend = new FakeBackend({ deferred: true });
  const { core, events } = observeCore(backend);
  core.start(makeConfig({ duration: { downloadMs: 10_000 } }), 0);
  advance(2_000);
  backend.prepared!();
  await Promise.resolve();
  advance(1_499);
  expect(hasEvent(events, "stall")).toBe(false);
  advance(2);
  expect(hasEvent(events, "stall")).toBe(true);
});

test("stall counts toward the window but blocks finalization until resume", async () => {
  const { core, events } = await startCore({ duration: { downloadMs: 1000 } });
  advance(300);
  let last = progressEvents(events).at(-1)!;
  expect(last.phaseElapsedMs).toBe(300);
  expect(last.measuring).toBe(true);
  core.stall({ reason: "connection-lost", detail: "test" });
  expect(hasEvent(events, "stall")).toBe(true);
  advance(500);
  last = progressEvents(events).at(-1)!;
  expect(last.phaseElapsedMs).toBe(800);
  expect(last.measuring).toBe(false);
  advance(200);
  expect(core.phase).toBe("download");
  core.resume();
  expect(hasEvent(events, "resume")).toBe(true);
  advance(20);
  expect(core.phase).toBe("complete");
});

test("latency presentation does not bridge a short stall", async () => {
  const { core, events } = await startCore({ duration: { downloadMs: 1_000 } });
  advance(10);
  core.ingestLatency({ rttMs: 10, lost: false, observedAtMs: fakeNow });
  core.stall({ reason: "connection-lost", detail: "test" });
  advance(100);
  core.resume();
  core.ingestLatency({ rttMs: 12, lost: false, observedAtMs: fakeNow });
  advance(300);
  core.ingestLatency({ rttMs: 14, lost: false, observedAtMs: fakeNow });
  advance(250);
  const buckets = eventSamples(events, "latency");
  expect(buckets.length).toBeGreaterThanOrEqual(2);
  expect(buckets[0].continuityId).not.toBe(buckets.at(-1)!.continuityId);
});

test("latency presentation closes on bucket time without a later ping", async () => {
  const { core, events } = await startCore({ duration: { downloadMs: 1_000 } });
  advance(10);
  core.ingestLatency({ rttMs: 20, lost: false, observedAtMs: fakeNow });
  expect(hasEvent(events, "latency")).toBe(false);
  advance(240);
  const latency = eventSamples(events, "latency")[0];
  expect(latency).toMatchObject({
    startT: 0,
    endT: latencyPresentationBucketMs(1_000, 250),
    medianRttMs: 20,
  });
});

test.each(["fast", "medium", "slow"] as const)(
  "loaded latency presentation follows %s cadence",
  async (cadence) => {
    const durationMs = 2_400;
    const pingIntervalMs = fixedPingIntervalMs(cadence)!;
    const { core, events } = await startCore({
      loadedPingCadence: cadence,
      duration: { downloadMs: durationMs },
    });
    for (let t = 0; t < durationMs; t += pingIntervalMs) {
      fakeNow = t;
      core.ingestThroughput("down", 100, pingIntervalMs / 1_000);
      core.ingestLatency({ rttMs: 20, lost: false, observedAtMs: t });
      advance(0);
    }
    fakeNow = durationMs;
    advance(0);
    const samples = eventSamples(events, "latency");
    const bucketMs = latencyPresentationBucketMs(durationMs, pingIntervalMs);
    expect(samples).toHaveLength(Math.ceil(durationMs / bucketMs));
    expect(samples.every((sample) => sample.pingCount > 0)).toBe(true);
    expect(samples.map((sample) => sample.startT)).toEqual(
      samples.map((_, index) => index * bucketMs),
    );
  },
);

test("queued latency outcomes retain their worker observation buckets", async () => {
  const { core, events } = await startCore({
    stages: { latency: true, download: false },
    duration: { latencyMs: 1_000, downloadMs: 0 },
  });
  fakeNow = 400;
  core.ingestLatency({ rttMs: 10, lost: false, observedAtMs: 100 });
  core.ingestLatency({ rttMs: 20, lost: false, observedAtMs: 350 });
  advance(0);
  const buckets = eventSamples(events, "latency");
  expect(buckets.map((bucket) => bucket.startT)).toEqual([0, 200]);
  expect(buckets.map((bucket) => bucket.medianRttMs)).toEqual([10, 20]);
});

test("watchdog auto-stalls a measured phase after prolonged sample silence", async () => {
  const { events } = await startCore({ duration: { downloadMs: 10000 } });
  advance(800);
  expect(hasEvent(events, "stall")).toBe(false);
  advance(800);
  const stall = events.find((e) => e.type === "stall");
  expect(stall).toBeDefined();
  if (stall?.type === "stall") {
    expect(stall.info.reason).toBe("connection-lost");
  }
  const before = progressEvents(events).at(-1)!.phaseElapsedMs;
  advance(300);
  const after = progressEvents(events).at(-1)!.phaseElapsedMs;
  expect(after).toBeGreaterThan(before);
});

test("loaded pings do not hide a stalled transfer", async () => {
  const { backend, events } = await startCore({
    duration: { downloadMs: 5000 },
  });
  for (let i = 0; i < 8; i++) {
    backend.host.ingestLatency({
      rttMs: 2,
      lost: false,
      observedAtMs: fakeNow,
    });
    advance(250);
  }
  expect(hasEvent(events, "stall")).toBe(true);
});

test("backend boundary flush is included before stage reduction", async () => {
  const backend = new FakeBackend({ flush: "sync" });
  const { events } = await startCore(
    { duration: { downloadMs: 100 } },
    backend,
  );
  advance(100);
  expectComplete(events, (result) =>
    expect(result.download?.totalBytes).toBe(100),
  );
});

test("terminal probe accounting reaches the original stage summary before finalization", async () => {
  const backend = new FakeBackend({ flush: "async" });
  const { core, events } = await startCore(
    { duration: { downloadMs: 100 } },
    backend,
  );
  core.ingestLatency({
    rttMs: 10,
    reflectorHandlingMs: 2,
    lost: false,
    observedAtMs: fakeNow,
  });
  advance(100);
  expect(core.phase).toBe("download");
  expect(hasEvent(events, "complete")).toBe(false);
  core.ingestLatency({ rttMs: 250, lost: true, observedAtMs: fakeNow });
  core.ingestLatency({
    rttMs: 30,
    reflectorHandlingMs: 9,
    lost: false,
    observedAtMs: fakeNow + 20,
    rttEligible: false,
  });
  core.ingestLatencyInterruption(2, "unresolved");
  backend.flush!();
  await Promise.resolve();
  expect(core.phase).toBe("complete");
  expectComplete(events, (result) => {
    expect(result.download?.totalBytes).toBe(100);
    expect(result.latencyByStage.download).toMatchObject({
      probeCount: 3,
      timeoutCount: 1,
      unresolvedCount: 2,
      reflectorTiming: {
        sampleCount: 1,
        meanRawRttMs: 10,
        meanHandlingMs: 2,
        meanAdjustedRttMs: 8,
      },
      meanMs: 10,
      p50Ms: 10,
      jitterPairs: 0,
    });
    expect(result.latencyByStage.latency).toBeNull();
    expect(result.download?.probeTimeoutPct).toBeCloseTo(100 / 3);
  });
});

test("a failed terminal worker keeps unknown accounting visible in the final stage summary", async () => {
  const backend = new FakeBackend({ flush: "async" });
  const { core, events } = await startCore(
    { duration: { downloadMs: 100 } },
    backend,
  );
  advance(100);
  core.ingestLatencyAccountingIncomplete();
  backend.flush!();
  await Promise.resolve();
  expectComplete(events, (result) => {
    expect(result.latencyByStage.download).toMatchObject({
      accountingComplete: false,
      probeCount: 0,
      timeoutCount: 0,
      unresolvedCount: 0,
    });
    expect(result.download?.probeTimeoutPct).toBeNull();
  });
});

test("a real sample arriving mid-stall auto-resumes", async () => {
  const { core, events } = await startCore({ duration: { downloadMs: 10000 } });
  core.stall({ reason: "connection-lost" });
  expect(events.filter((e) => e.type === "stall").length).toBe(1);
  core.ingestThroughput("down", 0, 0.1);
  expect(hasEvent(events, "resume")).toBe(false);
  core.ingestThroughput("down", 100, 0.1);
  expect(hasEvent(events, "resume")).toBe(true);
});

test("a healthy sibling's bytes do not resume a stalled bidirectional stage", async () => {
  const { core, events } = await startCore({
    stages: { download: false, bidirectional: true },
    duration: { bidirectionalMs: 60_000 },
  });
  core.stall({ reason: "connection-lost" });
  core.ingestThroughput("down", 100, 0.1, false, false);
  expect(hasEvent(events, "resume")).toBe(false);
  advance(STAGE_RECOVERY_BUDGET_MS + 1);
  expect(core.phase).toBe("error");
  expect(typedEvents(events, "error")[0]?.error.reason).toBe("connection-lost");
});

test("accounting windows cannot overwrite the shared stall presentation", async () => {
  const { core, events } = await startCore({
    duration: { downloadMs: 10_000 },
  });
  advance(10);
  core.ingestThroughput("down", 100, 0.1);
  core.stall({ reason: "connection-lost" });
  advance(400);
  const before = events.filter((event) => event.type === "throughput");
  expect(before.at(-1)?.sample.bytesPerSec).toBe(500);
  core.ingestThroughput("down", 0, 0.1, false, false);
  const after = events.filter((event) => event.type === "throughput");
  expect(after).toHaveLength(before.length);
  expect(after.at(-1)?.sample.bytesPerSec).toBe(500);
});

test("a non-liveness throughput sample remains in the result", async () => {
  const { core, events } = await startCore({
    stages: { download: false, bidirectional: true },
    duration: { bidirectionalMs: 100 },
  });
  const complete = () => completeEvent(events);
  core.stall({ reason: "connection-lost" });
  core.ingestThroughput("down", 100, 0.1, false, false);
  core.resume();
  advance(100);
  expect(complete()?.result.bidirectional?.down?.totalBytes).toBe(100);
});

test("the runner owns recovery request lifetime", async () => {
  const { backend, core } = await startCore({
    stages: { download: false, upload: true },
    duration: { uploadMs: 1_000 },
  });
  core.stall({
    reason: "connection-lost",
    recoveryCause: "unknown-upload-id",
    direction: "up",
  });
  expect(backend.recoveries).toHaveLength(1);
  expect(backend.recoveries[0].stage).toBe("upload");
  expect(backend.recoveries[0].cause).toBe("unknown-upload-id");
  expect(backend.recoveries[0].direction).toBe("up");
  expect(backend.recoveries[0].signal.aborted).toBe(false);
  core.resume();
  expect(backend.recoveries[0].signal.aborted).toBe(true);
});

test("reply-driven latency bounds confidence work while retaining every measured outcome", async () => {
  const confidence = spyOn(RunAccumulator.prototype, "confidence");
  try {
    const { core, events } = await startCore({
      stages: { latency: true, download: false },
      duration: { latencyMs: 1_000, downloadMs: 0 },
    });
    advance(10);
    const initialCalls = confidence.mock.calls.length;
    for (let i = 0; i < 5_000; i++) {
      core.ingestLatency({ rttMs: 20, lost: false, observedAtMs: fakeNow });
    }
    expect(confidence.mock.calls.length - initialCalls).toBe(1);
    advance(100);
    core.ingestLatency({ rttMs: 40, lost: false, observedAtMs: fakeNow });
    expect(confidence.mock.calls.length - initialCalls).toBe(2);
    expect(typedEvents(events, "stability").at(-1)?.snapshot.sampleCount).toBe(
      5_001,
    );
    advance(890);
    expectComplete(events, (result) => {
      expect(result.latencyByStage.latency).toMatchObject({
        probeCount: 5_001,
        timeoutCount: 0,
        unresolvedCount: 0,
        p50Ms: 20,
      });
      expect(result.latencyByStage.latency?.meanMs).toBeCloseTo(
        100_040 / 5_001,
      );
    });
  } finally {
    confidence.mockRestore();
  }
});

test("latency confirmation checks outcomes received inside the confidence cadence", async () => {
  const confidence = spyOn(RunAccumulator.prototype, "confidence");
  try {
    const { core } = await startCore({
      stages: { latency: true, download: false },
      duration: { latencyMs: 1_000, downloadMs: 0 },
      adaptive: { enabled: true, minLatencySamples: 8, confirmationMs: 50 },
    });
    advance(10);
    for (let i = 0; i < 50; i++)
      core.ingestLatency({ rttMs: 20, lost: false, observedAtMs: fakeNow });
    advance(100);
    core.ingestLatency({ rttMs: 20, lost: false, observedAtMs: fakeNow });
    const callsWhenArmed = confidence.mock.calls.length;
    fakeNow += 25;
    for (let i = 0; i < 50; i++)
      core.ingestLatency({ rttMs: 20, lost: true, observedAtMs: fakeNow });
    expect(confidence.mock.calls).toHaveLength(callsWhenArmed);
    advance(25);
    expect(confidence.mock.calls).toHaveLength(callsWhenArmed + 1);
    expect(core.phase).toBe("latency");
  } finally {
    confidence.mockRestore();
  }
});

test("default latency policy can confirm early at the fixed slow cadence", async () => {
  const { core } = observeCore();
  const cfg = makeConfig({
    stages: { latency: true, download: false },
    duration: { latencyMs: 4_000, downloadMs: 0 },
    adaptive: {
      enabled: true,
      minCoverageRatio: 0.52,
      stabilityThreshold: 0.86,
      maxPhaseReductionRatio: 0.5,
      minLatencySamples: 8,
      confirmationMs: 1_100,
    },
  });
  cfg.pingCadence = "slow";
  core.start(cfg, 0);
  advance(10);
  core.ingestLatency({ rttMs: 20, lost: false, observedAtMs: fakeNow });
  for (let i = 1; i < 5; i++) {
    advance(600);
    core.ingestLatency({ rttMs: 20, lost: false, observedAtMs: fakeNow });
  }
  expect(core.phase).toBe("latency");
  advance(1_099);
  expect(core.phase).toBe("latency");
  advance(1);
  expect(core.phase).toBe("complete");
  expect(fakeNow).toBeLessThan(cfg.duration.latencyMs);
});

test("adaptive early-finish arms and completes the run well before the nominal duration on a stable feed", async () => {
  const { core, cfg, events } = await startStableDownload(2000, {
    minLatencySamples: 0,
    confirmationMs: 100,
  });
  let wallAdvanced = 0;
  advance(10);
  wallAdvanced += 10;
  feedFlatThroughput(core, 15);
  advance(10);
  wallAdvanced += 10;
  expect(core.phase).toBe("download");
  advance(100);
  wallAdvanced += 100;
  expect(core.phase).toBe("complete");
  expect(wallAdvanced).toBeLessThan(cfg.duration.downloadMs / 2);
  expectComplete(events, (result) => {
    expect(result.download?.method).toBe("stable-window");
    expect(result.download?.reportedBytesPerSec).toBeCloseTo(1000, 6);
    expect(result.download?.fullAverageBytesPerSec).toBeCloseTo(1000, 6);
  });
});

test("a throughput drop during confirmation revokes early completion", async () => {
  const { core, cfg } = await startStableDownload(5_000, {
    confirmationMs: 500,
  });
  advance(10);
  feedFlatThroughput(core, 15);
  for (let i = 0; i < 4; i++)
    core.ingestThroughput("down", 0, 0.25, false, false);
  advance(cfg.adaptive.confirmationMs);
  expect(core.phase).toBe("download");
});

test("a confirmed throughput regime change revokes early completion without breaking chart continuity", async () => {
  const { core, events } = await startStableDownload(10_000, {
    confirmationMs: 2_000,
  });
  advance(10);
  feedFlatThroughput(core, 30);
  feedFlatThroughput(core, 20, 40);
  const continuities = throughputContinuityIds(events);
  expect(new Set(continuities).size).toBe(1);
  advance(1_000);
  expect(core.phase).toBe("download");
});

test("a confirmed upward throughput regime change preserves chart continuity", async () => {
  const { core, events } = await startDownload(10_000);
  advance(10);
  feedFlatThroughput(core, 30, 40);
  feedFlatThroughput(core, 20);
  const continuities = throughputContinuityIds(events);
  expect(new Set(continuities).size).toBe(1);
});

test("an explicit stall and resume break throughput continuity", async () => {
  const { core, events } = await startDownload(10_000);
  advance(10);
  feedFlatThroughput(core, 1);
  const before = throughputContinuityIds(events).at(-1)!;
  core.stall({ reason: "connection-lost" });
  advance(100);
  feedFlatThroughput(core, 1);
  const after = throughputContinuityIds(events).at(-1)!;
  expect(after).toBeGreaterThan(before);
});

test("a stall during confirmation revokes early completion", async () => {
  const { core, cfg } = await startStableDownload(5_000, {
    confirmationMs: 500,
  });
  advance(10);
  feedFlatThroughput(core, 15);
  core.stall({ reason: "connection-lost", detail: "test" });
  advance(cfg.adaptive.confirmationMs);
  expect(core.phase).toBe("download");
});

test("adaptive completion off publishes the whole phase even when it ends stable", async () => {
  const { core, events } = await startDownload(500, {
    adaptive: { enabled: false },
  });
  for (let i = 0; i < 10; i++)
    core.ingestThroughput("down", i === 0 ? 1 : 100, 0.1);
  advance(500);
  expectComplete(events, (complete) => {
    const result = complete.download!;
    expect(result.method).toBe("full-average");
    expect(result.reportedBytesPerSec).toBeCloseTo(901, 6);
    expect(result.reportedBytesPerSec).toBe(result.fullAverageBytesPerSec);
  });
});

test("adaptive enabled does not select a stable tail when the nominal phase wins", async () => {
  const { core, events } = await startStableDownload(10_000, {
    minCoverageRatio: 0.25,
    confirmationMs: 10_000,
  });
  for (let i = 0; i < 40; i++) {
    core.ingestThroughput("down", 10, 0.1);
    advance(100);
  }
  for (let i = 0; i < 60; i++) {
    core.ingestThroughput("down", 100, 0.1);
    advance(100);
  }
  const stability = events.filter((event) => event.type === "stability").at(-1);
  expect(stability?.type === "stability" && stability.snapshot.band).toBe(
    "high",
  );
  expectComplete(events, (complete) => {
    const result = complete.download!;
    expect(result.method).toBe("full-average");
    expect(result.reportedBytesPerSec).toBeCloseTo(640, 6);
    expect(result.reportedBytesPerSec).toBe(result.fullAverageBytesPerSec);
  });
});

test("adaptive early-finish never arms on a noisy (monotonic ramp) feed — the phase runs to its nominal end", async () => {
  const { core } = await startStableDownload(5000, {
    minTransferSamples: 5,
    minLatencySamples: 0,
    confirmationMs: 50,
  });
  const N = 50;
  for (let i = 0; i < N; i++) {
    advance(100);
    const raw = 100 + i * 100;
    core.ingestThroughput("down", raw * 0.1, 0.1);
    if (i < N - 1) expect(core.phase).toBe("download"); // never armed early
  }
  expect(core.phase).toBe("complete");
});

test("duration changes resize the active stage and finish immediately when its new budget has passed", async () => {
  const { core, cfg } = await startDownload(2000);
  advance(600);
  reconfigureDownload(core, cfg, { duration: { downloadMs: 500 } });
  expect(core.phase).toBe("complete");
});

test("extending the active duration keeps the stage running to the new budget", async () => {
  const { core, cfg } = await startDownload(500);
  advance(400);
  reconfigureDownload(core, cfg, { duration: { downloadMs: 1000 } });
  advance(100);
  expect(core.phase).toBe("download");
  advance(500);
  expect(core.phase).toBe("complete");
});

test("extending duration below the new coverage floor revokes confirmation", async () => {
  const { core, cfg } = await startStableDownload(1_000, {
    minCoverageRatio: 0.5,
    confirmationMs: 200,
  });
  advance(600);
  feedFlatThroughput(core, 15);
  reconfigureDownload(core, cfg, { duration: { downloadMs: 2_000 } });
  advance(cfg.adaptive.confirmationMs);
  expect(core.phase).toBe("download");
});

test("adaptive completion can be enabled after a stable stage has started", async () => {
  const { core, cfg } = await startDownload(2000, {
    adaptive: { enabled: false, minTransferSamples: 4 },
  });
  advance(400);
  feedFlatThroughput(core, 10);
  reconfigureDownload(core, cfg, { adaptive: { enabled: true } });
  advance(cfg.adaptive.confirmationMs);
  expect(core.phase).toBe("complete");
});

test("shortening confirmation is re-evaluated immediately", async () => {
  const { core, cfg } = await startStableDownload(5_000, {
    confirmationMs: 1_000,
  });
  advance(10);
  feedFlatThroughput(core, 15);
  advance(300);
  expect(core.phase).toBe("download");
  reconfigureDownload(core, cfg, { adaptive: { confirmationMs: 200 } });
  expect(core.phase).toBe("complete");
});

test("adaptive completion can be disabled before a stable stage arms", async () => {
  const { core, cfg } = await startStableDownload(2000);
  advance(400);
  feedFlatThroughput(core, 10);
  reconfigureDownload(core, cfg, { adaptive: { enabled: false } });
  advance(cfg.adaptive.confirmationMs * 2);
  expect(core.phase).toBe("download");
});

test("raw samples reduce at source cadence while presentation snapshots are capped at about 60 ms", async () => {
  const { core, events } = await startCore();
  for (let i = 0; i < 50; i++) {
    fakeNow += 20;
    core.ingestThroughput("down", 20, 0.02);
  }
  expect(typedEvents(events, "throughput")).toHaveLength(17);
  advance(1000);
  const complete = completeEvent(events);
  expect(
    complete?.type === "complete" && complete.result.download?.totalBytes,
  ).toBe(1000);
});

test("presentation derives from receiver bytes and intervals while retaining exact totals", async () => {
  const { core, events } = await startCore({
    duration: { downloadMs: 100000 },
  });
  const DT = 200;
  const N = 12;
  const DELTA = 50;
  core.ingestThroughput("down", 0, 0.1);
  for (let i = 0; i < N; i++) {
    fakeNow += DT;
    core.ingestThroughput("down", DELTA, 0.1);
  }
  const throughputSamples = typedEvents(events, "throughput");
  const lastSample = throughputSamples.at(-1)!;
  expect(lastSample.sample.bytesPerSec).toBeCloseTo(500, 6);
  expect(lastSample.sample.bytesCumulative).toBe(N * DELTA);
  advance(1_000_000);
  core.resume();
  advance(20);
  expectComplete(events, (result) => {
    const effectiveRate = (N * DELTA) / ((N + 1) * 0.1);
    expect(result.download!.fullAverageBytesPerSec).toBeCloseTo(
      effectiveRate,
      3,
    );
    expect(result.download!.totalBytes).toBe(N * DELTA);
  });
});

test("worker observations retain their timeline position across delayed delivery and ticks", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const config = structuredClone(DEFAULT_CONFIG);
  config.adaptive.enabled = false;
  config.duration.warmupMs = 0;
  config.duration.latencyMs = 4000;
  core.start(config, 1);
  advance(200);
  expect(core.observationTime(195)).toBe(195);
  // A delivered batch takes time to consume before the next master tick.
  fakeNow = 275;
  expect(core.observationTime(195)).toBe(195);
  expect(core.observationTime(210)).toBe(210);
  advance(25);
  expect(core.observationTime(195)).toBe(195);
  core.dispose();
});
