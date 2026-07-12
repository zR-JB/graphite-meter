import { test, expect, beforeEach, afterEach } from "bun:test";
import { RunnerCore, type RunnerBackend, type CoreHost } from "./core";
import type {
  RunnerConfig,
  RunnerEvent,
  PhaseActivity,
  Phase,
  InfraInfo,
  EngineInfo,
} from "./contract";

// ---------------------------------------------------------------------------
// Fake clock + captured tick callback.
//
// core.ts drives everything off a real `setInterval(() => this.#tick(), 20)`
// plus `performance.now()`. Both are monkey-patched here so a test can drive
// the tick loop deterministically and instantly: `advance(ms)` fakes `ms` of
// wall-clock passing and fires exactly one master tick, mirroring one real
// `setInterval` callback firing after that much real time.
// ---------------------------------------------------------------------------
let fakeNow = 0;
let tickCallback: (() => void) | null = null;

const realNow = performance.now.bind(performance);
const realSetInterval = globalThis.setInterval;
const realClearInterval = globalThis.clearInterval;

beforeEach(() => {
  fakeNow = 0;
  tickCallback = null;
  performance.now = () => fakeNow;
  globalThis.setInterval = ((fn: () => void) => {
    tickCallback = fn;
    return 1 as unknown as ReturnType<typeof setInterval>;
  }) as typeof setInterval;
  globalThis.clearInterval = (() => {
    tickCallback = null;
  }) as typeof clearInterval;
});

afterEach(() => {
  performance.now = realNow;
  globalThis.setInterval = realSetInterval;
  globalThis.clearInterval = realClearInterval;
});

/** Advance the fake wall clock by `ms` and fire one master tick. */
function advance(ms: number): void {
  fakeNow += ms;
  tickCallback?.();
}

// ---------------------------------------------------------------------------
// Fake backend: a push-style RunnerBackend test double. It records the
// 3-call stage lifecycle (begin/measure/end) plus run-level calls, and leaves
// `onTick` undefined — samples arrive only when a test calls the CoreHost
// methods (ingestThroughput/ingestLatency/stall/resume) directly on the core,
// exactly like a real network backend pushing from its own callbacks.
// ---------------------------------------------------------------------------
class FakeBackend implements RunnerBackend {
  host!: CoreHost;
  calls: string[] = [];

  attach(host: CoreHost): void {
    this.host = host;
  }
  probe(): Promise<InfraInfo> {
    return Promise.resolve({
      clientIp: "127.0.0.1",
      server: { name: "fake", host: "fake", port: 0 },
      preTestPingMs: 0,
      engineVersion: "test",
      protocolNegotiated: "fake",
    });
  }
  describe(): EngineInfo {
    return {
      name: "fake",
      version: "0",
      latencyTransports: [],
      throughputTransports: [],
    };
  }
  onRunStart(): void {
    this.calls.push("runStart");
  }
  onStageBegin(activity: PhaseActivity): void {
    this.calls.push(`begin:${activity.stage}`);
  }
  onStageMeasure(activity: PhaseActivity): void {
    this.calls.push(`measure:${activity.stage}`);
  }
  onStageEnd(activity: PhaseActivity): void {
    this.calls.push(`end:${activity.stage}`);
  }
  onComplete(): void {
    this.calls.push("complete");
  }
  onAbort(): void {
    this.calls.push("abort");
  }
}

function makeConfig(
  overrides: {
    stages?: Partial<RunnerConfig["stages"]>;
    duration?: Partial<RunnerConfig["duration"]>;
    adaptive?: Partial<RunnerConfig["adaptive"]>;
  } = {},
): RunnerConfig {
  return {
    stages: {
      latency: false,
      download: true,
      upload: false,
      bidirectional: false,
      ...overrides.stages,
    },
    skipLoadedLatencyWhenStageOff: true,
    duration: {
      warmupMs: 0,
      latencyMs: 0,
      downloadMs: 1000,
      uploadMs: 0,
      bidirectionalMs: 0,
      ...overrides.duration,
    },
    pingConcurrency: "medium",
    parallelStreams: 6,
    experimentalChunkedDownload: false,
    endpoint: { host: "auto", port: 443 },
    compensation: {
      enabled: false,
      profile: "lan",
      transport: "http1-clear",
      factors: {
        ethernetFraming: false,
        encapsulation: false,
        tlsRecords: false,
        applicationFraming: false,
        reversePathControl: false,
        lossRetransmission: false,
        receiverBias: false,
        steadyStateRamp: false,
        browserRuntime: false,
      },
      params: {
        mtuBytes: 1500,
        ipVersion: 4,
        vlanTagged: false,
        tcpOptionsBytes: 12,
        encapsulationBytes: 60,
        framePayloadBytes: 16384,
        tlsRecordBytes: 5,
        aeadTagBytes: 16,
        quicConnIdBytes: 8,
        maxLossRatio: 0.12,
      },
    },
    adaptive: {
      enabled: false,
      minCoverageRatio: 0,
      stabilityThreshold: 0.9,
      maxPhaseReductionRatio: 1,
      minLatencySamples: 0,
      minTransferSamples: 0,
      glideMs: 100,
      ...overrides.adaptive,
    },
    visualization: { throughputMaxBytesPerSec: "auto" },
  };
}

function phaseTransitions(events: RunnerEvent[]): Phase[] {
  const seq: Phase[] = [];
  for (const e of events) if (e.type === "phase") seq.push(e.transition.to);
  return seq;
}

function progressEvents(
  events: RunnerEvent[],
): Extract<RunnerEvent, { type: "progress" }>[] {
  return events.filter(
    (e): e is Extract<RunnerEvent, { type: "progress" }> =>
      e.type === "progress",
  );
}

// ---------------------------------------------------------------------------
// Phase timeline + stage lifecycle
// ---------------------------------------------------------------------------

test("full run: latency then download — phase order and stage lifecycle", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({
    stages: { latency: true, download: true },
    duration: { latencyMs: 100, downloadMs: 100 },
  });
  core.start(cfg);

  expect(backend.calls).toEqual([
    "runStart",
    "begin:latency",
    "measure:latency",
  ]);
  expect(phaseTransitions(events)).toEqual(["latency"]);

  advance(100); // crosses into download
  expect(backend.calls).toEqual([
    "runStart",
    "begin:latency",
    "measure:latency",
    "end:latency",
    "begin:download",
    "measure:download",
  ]);
  expect(phaseTransitions(events)).toEqual(["latency", "download"]);
  expect(core.phase).toBe("download");

  advance(100); // reaches totalMs -> finish
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

  const complete = events.find((e) => e.type === "complete");
  expect(complete).toBeDefined();
  if (complete?.type === "complete") {
    expect(complete.result.download).not.toBeNull();
    expect(complete.result.latency).not.toBeNull();
  }
});

test("warmup->measure seam: same stage, no onStageEnd between begin and measure", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({
    duration: { warmupMs: 50, downloadMs: 100 },
  });
  core.start(cfg);

  // Warmup begins the stage but does not measure yet.
  expect(backend.calls).toEqual(["runStart", "begin:download"]);
  expect(phaseTransitions(events)).toEqual(["warmup"]);

  advance(50); // warmup window elapses -> same stage starts measuring
  expect(backend.calls).toEqual([
    "runStart",
    "begin:download",
    "measure:download",
  ]);
  expect(phaseTransitions(events)).toEqual(["warmup", "download"]);

  advance(100); // finish
  expect(backend.calls).toEqual([
    "runStart",
    "begin:download",
    "measure:download",
    "end:download",
    "complete",
  ]);
});

// ---------------------------------------------------------------------------
// Measured test-time clock: stalls count, but cannot finalize a phase
// ---------------------------------------------------------------------------

test("stall counts toward the window but blocks finalization until resume", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({ duration: { downloadMs: 1000 } });
  core.start(cfg); // t=0, enters download

  advance(300);
  let last = progressEvents(events).at(-1)!;
  expect(last.phaseElapsedMs).toBe(300);
  expect(last.measuring).toBe(true);

  core.stall({ reason: "connection-lost", detail: "test" });
  expect(events.some((e) => e.type === "stall")).toBe(true);

  // Wall time continues through the stall.
  advance(500);
  last = progressEvents(events).at(-1)!;
  expect(last.phaseElapsedMs).toBe(800);
  expect(last.measuring).toBe(false);
  advance(200); // budget reached while stalled
  expect(core.phase).toBe("download");

  core.resume();
  expect(events.some((e) => e.type === "resume")).toBe(true);

  advance(20);
  expect(core.phase).toBe("complete");
});

test("watchdog auto-stalls a measured phase after prolonged sample silence", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({ duration: { downloadMs: 10000 } });
  core.start(cfg);

  advance(800); // under the 1500ms watchdog threshold
  expect(events.some((e) => e.type === "stall")).toBe(false);

  advance(800); // cumulative silence now exceeds 1500ms
  const stall = events.find((e) => e.type === "stall");
  expect(stall).toBeDefined();
  if (stall?.type === "stall") {
    expect(stall.info.reason).toBe("connection-lost");
  }

  // The effective-throughput clock continues through the stalled interval.
  const before = progressEvents(events).at(-1)!.phaseElapsedMs;
  advance(300);
  const after = progressEvents(events).at(-1)!.phaseElapsedMs;
  expect(after).toBeGreaterThan(before);
});

test("loaded pings do not hide a stalled transfer", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));
  core.start(makeConfig({ duration: { downloadMs: 5000 } }));

  for (let i = 0; i < 8; i++) {
    backend.host.ingestLatency(2, true, false);
    advance(250);
  }

  expect(events.some((e) => e.type === "stall")).toBe(true);
});

test("backend boundary flush is included before stage reduction", () => {
  class FlushingBackend extends FakeBackend {
    override onStageEnd(activity: PhaseActivity): void {
      if (activity.stage === "download")
        this.host.ingestThroughput("down", 1000, 100, 0.1);
      super.onStageEnd(activity);
    }
  }
  const backend = new FlushingBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));
  core.start(makeConfig({ duration: { downloadMs: 100 } }));
  advance(100);

  const complete = events.find((e) => e.type === "complete");
  expect(
    complete?.type === "complete" && complete.result.download?.totalBytes,
  ).toBe(100);
});

test("a real sample arriving mid-stall auto-resumes", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({ duration: { downloadMs: 10000 } });
  core.start(cfg);

  core.stall({ reason: "connection-lost" });
  expect(events.filter((e) => e.type === "stall").length).toBe(1);

  core.ingestThroughput("down", 0, 0, 0.1);
  expect(events.some((e) => e.type === "resume")).toBe(false);
  core.ingestThroughput("down", 1000, 100, 0.1);
  expect(events.some((e) => e.type === "resume")).toBe(true);
});

test("a stall that outlives max-stall escalates to a terminal failure", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({ duration: { downloadMs: 60000 } });
  core.start(cfg);

  core.stall({ reason: "connection-lost" });
  advance(20001); // exceeds MAX_STALL_MS (20000)

  expect(core.phase).toBe("error");
  const err = events.find((e) => e.type === "error");
  expect(err).toBeDefined();
  if (err?.type === "error") {
    expect(err.error.reason).toBe("connection-lost");
  }
});

// ---------------------------------------------------------------------------
// Adaptive early-finish glide
// ---------------------------------------------------------------------------

test("adaptive early-finish arms and completes the run well before the nominal duration on a stable feed", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({
    duration: { downloadMs: 2000 },
    adaptive: {
      enabled: true,
      minCoverageRatio: 0,
      stabilityThreshold: 0.9,
      maxPhaseReductionRatio: 1,
      minTransferSamples: 5,
      minLatencySamples: 0,
      glideMs: 100,
    },
  });
  core.start(cfg); // enters download at elapsed 0

  let wallAdvanced = 0;
  advance(10);
  wallAdvanced += 10;

  // A perfectly flat feed drives the confidence score to 1 (no variance, no
  // slope), well above the 0.9 threshold, once enough samples are in.
  for (let i = 0; i < 10; i++) core.ingestThroughput("down", 1000, 100, 0.1);

  advance(10); // this tick's confidence check arms the glide
  wallAdvanced += 10;
  expect(core.phase).toBe("download");

  advance(100); // == glideMs: the glide should drive measured-time to seg.end
  wallAdvanced += 100;

  expect(core.phase).toBe("complete");
  // The run finished after ~120ms of wall time, nowhere near the 2000ms budget.
  expect(wallAdvanced).toBeLessThan(cfg.duration.downloadMs / 2);
});

test("adaptive early-finish never arms on a noisy (monotonic ramp) feed — the phase runs to its nominal end", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({
    duration: { downloadMs: 5000 },
    adaptive: {
      enabled: true,
      minCoverageRatio: 0,
      stabilityThreshold: 0.9,
      maxPhaseReductionRatio: 1,
      minTransferSamples: 5,
      minLatencySamples: 0,
      glideMs: 50,
    },
  });
  core.start(cfg);

  // A steady ramp (never plateaus) keeps both the variance and the
  // first-vs-last-third slope of the confidence window high, so the
  // stability score never reaches the 0.9 gate — unlike the flat feed above.
  const N = 50;
  for (let i = 0; i < N; i++) {
    advance(100);
    const raw = 100 + i * 100;
    core.ingestThroughput("down", raw, raw, 1);
    if (i < N - 1) expect(core.phase).toBe("download"); // never armed early
  }

  // The run only reaches "complete" once the ramp has consumed the full
  // nominal 5000ms budget (fakeNow tracks exactly N * 100 = 5000 here).
  expect(core.phase).toBe("complete");
});

// ---------------------------------------------------------------------------
// Dual EMA (display vs stability) from the same raw samples
// ---------------------------------------------------------------------------

test("display (fast) and stability (slow) EMAs both derive from the same raw samples without drifting from exact totals", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({ duration: { downloadMs: 100000 } });
  core.start(cfg); // enters download at elapsed 0

  const DT = 200;
  const RAW = 1000;
  const N = 12;
  const DELTA = 50;

  // First sample seeds both EMA stores directly at 0 (raw=0), then a step to
  // a constant RAW value lets the two taus visibly diverge while converging.
  core.ingestThroughput("down", 0, 0, 0.1);
  let expectedFast = 0;
  const alphaFast = 1 - Math.exp(-DT / 700);
  const alphaSlow = 1 - Math.exp(-DT / 1800);
  let expectedSlow = 0;

  for (let i = 0; i < N; i++) {
    fakeNow += DT; // advance the mocked clock feeding #emaStep's dt directly
    core.ingestThroughput("down", RAW, DELTA, 0.1);
    expectedFast = expectedFast + alphaFast * (RAW - expectedFast);
    expectedSlow = expectedSlow + alphaSlow * (RAW - expectedSlow);
  }

  const throughputSamples = events.filter(
    (e): e is Extract<RunnerEvent, { type: "throughput" }> =>
      e.type === "throughput",
  );
  const lastSample = throughputSamples.at(-1)!;

  // Display (fast tau) matches the independently-computed fast EMA.
  expect(lastSample.sample.bytesPerSec).toBeCloseTo(expectedFast, 6);
  // Fast tau tracks the step closer than slow tau at the same sample index.
  expect(lastSample.sample.bytesPerSec).toBeGreaterThan(expectedSlow);

  // Raw byte totals are exact and untouched by either smoothing: N steps of
  // DELTA plus the seed sample's 0 bytes.
  expect(lastSample.sample.bytesCumulative).toBe(N * DELTA);

  // Finish the run and verify the headline uses exact bytes / represented time,
  // while the slow EMA remains stability-only.
  advance(1_000_000);
  core.resume();
  advance(20);
  const complete = events.find((e) => e.type === "complete");
  expect(complete).toBeDefined();
  if (complete?.type === "complete") {
    const effectiveRate = (N * DELTA) / ((N + 1) * 0.1);
    expect(complete.result.download!.fullAverageBytesPerSec).toBeCloseTo(
      effectiveRate,
      3,
    );
    expect(complete.result.download!.totalBytes).toBe(N * DELTA);
  }
});
