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
import { LATENCY_PRESENTATION_BUCKET_MS } from "./latencyBuckets";

// ---------------------------------------------------------------------------
// Fake clock + captured tick callback.
//
// The runner uses one deadline timer plus the monotonic clock. Both are patched
// so tests advance the measured clock deterministically.
// ---------------------------------------------------------------------------
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

/** Advance the fake wall clock by `ms` and fire one master tick. */
function advance(ms: number): void {
  fakeNow += ms;
  tickCallback?.();
}

// Records stage lifecycle calls; tests push samples through its host.
class FakeBackend implements RunnerBackend {
  host!: CoreHost;
  calls: string[] = [];

  attach(host: CoreHost): void {
    this.host = host;
  }
  probe(): Promise<InfraInfo> {
    return Promise.resolve({
      clientIp: "127.0.0.1",
      clientIpVersion: 4,
      clientIpSource: "socket",
      server: { name: "fake", host: "fake", port: 0 },
      preTestPingMs: 0,
      engineVersion: "test",
      discoveryGeneration: "test",
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
  onStageBegin(activity: PhaseActivity): void | Promise<void> {
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
  setBackgroundActivity(enabled: boolean): void {
    this.calls.push(`background:${enabled}`);
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
    pingCadence: "reply-driven",
    loadedPingCadence: "medium",
    transferStreams: { mode: "auto", count: 6 },
    experimentalChunkedDownload: false,
    experimentalDatagramThroughput: false,
    transports: {
      throughputTarget: "current",
      latencyTarget: "auto",
    },
    compensation: {
      profile: "lan",
      transport: "auto",
      params: {
        mtuBytes: 1500,
        ipVersion: 4,
        vlanTagged: false,
        tcpOptionsMinBytes: 0,
        tcpOptionsMaxBytes: 12,
        encapsulationBytes: 0,
        quicConnIdMinBytes: 0,
        quicConnIdMaxBytes: 20,
      },
    },
    adaptive: {
      enabled: false,
      minCoverageRatio: 0,
      stabilityThreshold: 0.9,
      maxPhaseReductionRatio: 1,
      minLatencySamples: 0,
      minTransferSamples: 0,
      confirmationMs: 100,
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

test("full run: latency then download — phase order and stage lifecycle", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({
    stages: { latency: true, download: true },
    duration: { latencyMs: 100, downloadMs: 100 },
  });
  await core.start(cfg);

  expect(backend.calls).toEqual([
    "runStart",
    "begin:latency",
    "measure:latency",
  ]);
  expect(phaseTransitions(events)).toEqual(["connecting", "latency"]);

  advance(100); // crosses into download
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

test("throughput stays isolated across transfer warmups", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  await core.start(
    makeConfig({
      stages: { download: true, upload: true, bidirectional: true },
      duration: {
        warmupMs: 100,
        downloadMs: 100,
        uploadMs: 100,
        bidirectionalMs: 100,
      },
    }),
  );

  advance(100); // download measurement
  core.ingestThroughput("down", 20_000_000, 2_000_000, 0.1);

  advance(100); // upload warmup
  expect(core.phase).toBe("warmup");
  core.ingestThroughput("down", 20_000_000, 2_000_000, 0.1);

  advance(100); // upload measurement
  core.ingestThroughput("up", 10_000_000, 1_000_000, 0.1);

  advance(100); // bidirectional warmup
  expect(core.phase).toBe("warmup");
  core.ingestThroughput("up", 10_000_000, 1_000_000, 0.1);

  advance(100); // bidirectional measurement
  core.ingestThroughput("down", 7_000_000, 700_000, 0.1);
  core.ingestThroughput("up", 3_000_000, 300_000, 0.1);

  const samples = events.flatMap((event) =>
    event.type === "throughput" ? [event.sample] : [],
  );
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
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  await core.start(
    makeConfig({
      stages: { download: true, upload: true },
      duration: { warmupMs: 100, downloadMs: 100, uploadMs: 100 },
    }),
  );
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
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({
    duration: { warmupMs: 50, downloadMs: 100 },
  });
  await core.start(cfg);

  // Warmup begins the stage but does not measure yet.
  expect(backend.calls).toEqual(["runStart", "begin:download"]);
  expect(phaseTransitions(events)).toEqual(["connecting", "warmup"]);

  advance(50); // warmup window elapses -> same stage starts measuring
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

  advance(100); // finish
  expect(backend.calls).toEqual([
    "runStart",
    "begin:download",
    "measure:download",
    "end:download",
    "complete",
  ]);
});

test("target verification is a visible phase and abort prevents a late run start", async () => {
  let resolveProbe!: (info: InfraInfo) => void;
  class PendingProbeBackend extends FakeBackend {
    override probe(): Promise<InfraInfo> {
      return new Promise((resolve) => (resolveProbe = resolve));
    }
  }
  const backend = new PendingProbeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const start = core.start(makeConfig());
  expect(core.phase).toBe("connecting");
  expect(phaseTransitions(events)).toEqual(["connecting"]);

  core.abort();
  resolveProbe(await new FakeBackend().probe());
  await start;

  expect(core.phase).toBe("aborted");
  expect(backend.calls).toEqual(["abort"]);
});

// The app always hands start() a prepared selection, so this is the only cover
// the internal probe has: the branch reads as dead from the app alone, and
// NetworkRunner still promises it to a caller that has not probed.
test("start without a prepared selection probes for one itself", async () => {
  class CountingProbeBackend extends FakeBackend {
    probes = 0;
    override probe(): Promise<InfraInfo> {
      this.probes++;
      return super.probe();
    }
  }
  const backend = new CountingProbeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  await core.start(makeConfig());

  expect(backend.probes).toBe(1);
  expect(events.filter((e) => e.type === "infra")).toHaveLength(1);
});

test("a prepared selection starts without probing again", async () => {
  class PreparedBackend extends FakeBackend {
    override probe(): Promise<InfraInfo> {
      throw new Error("unexpected probe");
    }
  }
  const backend = new PreparedBackend();
  const prepared = await new FakeBackend().probe();
  const core = new RunnerCore(backend);

  await core.start(makeConfig(), prepared);

  expect(backend.calls.slice(0, 2)).toEqual(["runStart", "begin:download"]);
});

test("asynchronous stage preparation cannot consume the warmup budget", async () => {
  let prepared!: () => void;
  class PreparingBackend extends FakeBackend {
    override onStageBegin(activity: PhaseActivity): Promise<void> {
      super.onStageBegin(activity);
      return new Promise((resolve) => (prepared = resolve));
    }
  }
  const backend = new PreparingBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  await core.start(
    makeConfig({ duration: { warmupMs: 100, downloadMs: 100 } }),
  );
  advance(1000);
  expect(core.phase).toBe("warmup");
  expect(progressEvents(events).at(-1)?.phaseElapsedMs).toBe(0);
  expect(backend.calls).toEqual(["runStart", "begin:download"]);

  prepared();
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

// ---------------------------------------------------------------------------
// Measured test-time clock: stalls count, but cannot finalize a phase
// ---------------------------------------------------------------------------

test("stall counts toward the window but blocks finalization until resume", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({ duration: { downloadMs: 1000 } });
  await core.start(cfg); // t=0, enters download

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

test("latency presentation does not bridge a short stall", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((event) => events.push(event));
  await core.start(makeConfig({ duration: { downloadMs: 1_000 } }));

  advance(10);
  core.ingestLatency(10, true, false);
  core.stall({ reason: "connection-lost", detail: "test" });
  advance(100); // below the chart's 600 ms natural-gap threshold
  core.resume();
  core.ingestLatency(12, true, false);
  advance(200);
  core.ingestLatency(14, true, false);

  const buckets = events.flatMap((event) =>
    event.type === "latency" ? [event.sample] : [],
  );
  expect(buckets.length).toBeGreaterThanOrEqual(2);
  expect(buckets[0].continuityId).not.toBe(buckets.at(-1)!.continuityId);
});

test("latency presentation closes on bucket time without a later ping", async () => {
  const core = new RunnerCore(new FakeBackend());
  const events: RunnerEvent[] = [];
  core.on((event) => events.push(event));
  await core.start(makeConfig({ duration: { downloadMs: 1_000 } }));

  advance(10);
  core.ingestLatency(20, true, false);
  expect(events.some((event) => event.type === "latency")).toBe(false);
  advance(190);

  const latency = events.find((event) => event.type === "latency");
  expect(latency?.sample).toMatchObject({
    startT: 0,
    endT: LATENCY_PRESENTATION_BUCKET_MS,
    medianRttMs: 20,
  });
});

test("watchdog auto-stalls a measured phase after prolonged sample silence", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({ duration: { downloadMs: 10000 } });
  await core.start(cfg);

  advance(800); // under the 1500ms watchdog threshold
  expect(events.some((e) => e.type === "stall")).toBe(false);

  advance(800); // cumulative silence exceeds 1500ms
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

test("loaded pings do not hide a stalled transfer", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));
  await core.start(makeConfig({ duration: { downloadMs: 5000 } }));

  for (let i = 0; i < 8; i++) {
    backend.host.ingestLatency(2, true, false);
    advance(250);
  }

  expect(events.some((e) => e.type === "stall")).toBe(true);
});

test("backend boundary flush is included before stage reduction", async () => {
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
  await core.start(makeConfig({ duration: { downloadMs: 100 } }));
  advance(100);

  const complete = events.find((e) => e.type === "complete");
  expect(
    complete?.type === "complete" && complete.result.download?.totalBytes,
  ).toBe(100);
});

test("asynchronous boundary flush completes before stage reduction", async () => {
  let flushed!: () => void;
  class FlushingBackend extends FakeBackend {
    override onStageEnd(activity: PhaseActivity): Promise<void> {
      super.onStageEnd(activity);
      return new Promise((resolve) => {
        flushed = () => {
          this.host.ingestThroughput("down", 1000, 100, 0.1);
          resolve();
        };
      });
    }
  }
  const backend = new FlushingBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));
  await core.start(makeConfig({ duration: { downloadMs: 100 } }));
  advance(100);

  expect(core.phase).toBe("download");
  expect(events.some((e) => e.type === "complete")).toBe(false);
  flushed();
  await Promise.resolve();

  const complete = events.find((e) => e.type === "complete");
  expect(core.phase).toBe("complete");
  expect(
    complete?.type === "complete" && complete.result.download?.totalBytes,
  ).toBe(100);
});

test("a real sample arriving mid-stall auto-resumes", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({ duration: { downloadMs: 10000 } });
  await core.start(cfg);

  core.stall({ reason: "connection-lost" });
  expect(events.filter((e) => e.type === "stall").length).toBe(1);

  core.ingestThroughput("down", 0, 0, 0.1);
  expect(events.some((e) => e.type === "resume")).toBe(false);
  core.ingestThroughput("down", 1000, 100, 0.1);
  expect(events.some((e) => e.type === "resume")).toBe(true);
});

test("a healthy sibling's bytes do not resume a stalled bidirectional stage", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  await core.start(
    makeConfig({
      stages: { download: false, bidirectional: true },
      duration: { bidirectionalMs: 60_000 },
    }),
  );
  core.stall({ reason: "connection-lost" });

  // Download still moves and must remain accounted, but upload is stalled, so
  // this sample cannot validate the combined stage or refresh its watchdog.
  core.ingestThroughput("down", 1000, 100, 0.1, false, false);
  expect(events.some((e) => e.type === "resume")).toBe(false);

  advance(20_001);
  expect(core.phase).toBe("error");
});

test("accounting windows cannot overwrite the shared stall presentation", async () => {
  const core = new RunnerCore(new FakeBackend());
  const events: RunnerEvent[] = [];
  core.on((event) => events.push(event));
  await core.start(makeConfig({ duration: { downloadMs: 10_000 } }));

  advance(10);
  core.ingestThroughput("down", 1_000, 100, 0.1);
  core.stall({ reason: "connection-lost" });
  advance(400);

  const before = events.filter((event) => event.type === "throughput");
  expect(before.at(-1)?.sample.bytesPerSec).toBe(500);
  core.ingestThroughput("down", 0, 0, 0.1, false, false);
  const after = events.filter((event) => event.type === "throughput");

  expect(after).toHaveLength(before.length);
  expect(after.at(-1)?.sample.bytesPerSec).toBe(500);
});

test("a non-liveness throughput sample remains in the result", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  let complete: Extract<RunnerEvent, { type: "complete" }> | undefined;
  core.on((event) => {
    if (event.type === "complete") complete = event;
  });

  await core.start(
    makeConfig({
      stages: { download: false, bidirectional: true },
      duration: { bidirectionalMs: 100 },
    }),
  );
  core.stall({ reason: "connection-lost" });
  core.ingestThroughput("down", 1000, 100, 0.1, false, false);
  core.resume();
  advance(100);

  expect(complete?.result.bidirectional?.down.totalBytes).toBe(100);
});

test("a stall that outlives max-stall escalates to a terminal failure", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({ duration: { downloadMs: 60000 } });
  await core.start(cfg);

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
// Adaptive early-finish confirmation
// ---------------------------------------------------------------------------

test("adaptive early-finish arms and completes the run well before the nominal duration on a stable feed", async () => {
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
      minTransferSamples: 4,
      minLatencySamples: 0,
      confirmationMs: 100,
    },
  });
  await core.start(cfg); // enters download at elapsed 0

  let wallAdvanced = 0;
  advance(10);
  wallAdvanced += 10;

  // A perfectly flat exact feed drives the fixed 250 ms buckets to confidence
  // 1. Keep collecting after the latch opens so the trailing exact reducer has
  // post-latch evidence during the confirmation interval.
  for (let i = 0; i < 15; i++) core.ingestThroughput("down", 1000, 100, 0.1);

  advance(10); // confirmation remains armed while evidence stays stable
  wallAdvanced += 10;
  expect(core.phase).toBe("download");

  advance(100); // confirmation closes at real measured time
  wallAdvanced += 100;

  expect(core.phase).toBe("complete");
  // The run finishes in ~120ms of wall time, without fabricating the 2000ms budget.
  expect(wallAdvanced).toBeLessThan(cfg.duration.downloadMs / 2);
  const complete = events.find((event) => event.type === "complete");
  expect(complete).toBeDefined();
  if (complete?.type === "complete") {
    expect(complete.result.download?.method).toBe("stable-window");
    expect(complete.result.download?.reportedBytesPerSec).toBeCloseTo(1000, 6);
    expect(complete.result.download?.fullAverageBytesPerSec).toBeCloseTo(
      1000,
      6,
    );
  }
});

test("a throughput drop during confirmation revokes early completion", async () => {
  const core = new RunnerCore(new FakeBackend());
  const cfg = makeConfig({
    duration: { downloadMs: 5_000 },
    adaptive: {
      enabled: true,
      minCoverageRatio: 0,
      stabilityThreshold: 0.9,
      maxPhaseReductionRatio: 1,
      minTransferSamples: 4,
      confirmationMs: 500,
    },
  });
  await core.start(cfg);
  advance(10);
  for (let i = 0; i < 15; i++) core.ingestThroughput("down", 1_000, 100, 0.1);

  // Four exact zero-rate control buckets invalidate the stable trace without
  // advancing wall time far enough to trigger the stall watchdog.
  for (let i = 0; i < 4; i++)
    core.ingestThroughput("down", 0, 0, 0.25, false, false);
  advance(cfg.adaptive.confirmationMs);

  expect(core.phase).toBe("download");
});

test("a confirmed throughput regime change revokes early completion", async () => {
  const core = new RunnerCore(new FakeBackend());
  const events: RunnerEvent[] = [];
  core.on((event) => events.push(event));
  const cfg = makeConfig({
    duration: { downloadMs: 10_000 },
    adaptive: {
      enabled: true,
      minCoverageRatio: 0,
      stabilityThreshold: 0.9,
      maxPhaseReductionRatio: 1,
      minTransferSamples: 4,
      confirmationMs: 2_000,
    },
  });
  await core.start(cfg);
  advance(10);
  for (let i = 0; i < 30; i++) core.ingestThroughput("down", 1_000, 100, 0.1);
  for (let i = 0; i < 20; i++) core.ingestThroughput("down", 400, 40, 0.1);

  const continuities = events.flatMap((event) =>
    event.type === "throughput" ? [event.sample.continuityId] : [],
  );
  expect(new Set(continuities).size).toBeGreaterThan(1);
  advance(1_000);
  expect(core.phase).toBe("download");
});

test("a stall during confirmation revokes early completion", async () => {
  const core = new RunnerCore(new FakeBackend());
  const cfg = makeConfig({
    duration: { downloadMs: 5_000 },
    adaptive: {
      enabled: true,
      minCoverageRatio: 0,
      stabilityThreshold: 0.9,
      maxPhaseReductionRatio: 1,
      minTransferSamples: 4,
      confirmationMs: 500,
    },
  });
  await core.start(cfg);
  advance(10);
  for (let i = 0; i < 15; i++) core.ingestThroughput("down", 1_000, 100, 0.1);

  core.stall({ reason: "connection-lost", detail: "test" });
  advance(cfg.adaptive.confirmationMs);

  expect(core.phase).toBe("download");
});

test("adaptive completion off publishes the whole phase even when it ends stable", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((event) => events.push(event));

  const cfg = makeConfig({
    duration: { downloadMs: 500 },
    adaptive: { enabled: false },
  });
  await core.start(cfg);
  for (let i = 0; i < 10; i++)
    core.ingestThroughput("down", 1000, i === 0 ? 1 : 100, 0.1);
  advance(500);

  const complete = events.find((event) => event.type === "complete");
  expect(complete).toBeDefined();
  if (complete?.type === "complete") {
    const result = complete.result.download!;
    expect(result.method).toBe("full-average");
    expect(result.reportedBytesPerSec).toBeCloseTo(901, 6);
    expect(result.reportedBytesPerSec).toBe(result.fullAverageBytesPerSec);
  }
});

test("adaptive early-finish never arms on a noisy (monotonic ramp) feed — the phase runs to its nominal end", async () => {
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
      confirmationMs: 50,
    },
  });
  await core.start(cfg);

  // A steady ramp (never plateaus) keeps both the variance and the
  // first-vs-last-third slope of the confidence window high, so the
  // stability score never reaches the 0.9 gate, unlike the flat feed above.
  const N = 50;
  for (let i = 0; i < N; i++) {
    advance(100);
    const raw = 100 + i * 100;
    core.ingestThroughput("down", raw, raw * 0.1, 0.1);
    if (i < N - 1) expect(core.phase).toBe("download"); // never armed early
  }

  // The run only reaches "complete" once the ramp has consumed the full
  // nominal 5000ms budget (fakeNow tracks exactly N * 100 = 5000 here).
  expect(core.phase).toBe("complete");
});

test("duration changes resize the active stage and finish immediately when its new budget has passed", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const cfg = makeConfig({ duration: { downloadMs: 2000 } });
  await core.start(cfg);
  advance(600);

  core.reconfigure({
    stages: cfg.stages,
    duration: { ...cfg.duration, downloadMs: 500 },
    adaptive: cfg.adaptive,
  });

  expect(core.phase).toBe("complete");
});

test("extending the active duration keeps the stage running to the new budget", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const cfg = makeConfig({ duration: { downloadMs: 500 } });
  await core.start(cfg);
  advance(400);

  core.reconfigure({
    stages: cfg.stages,
    duration: { ...cfg.duration, downloadMs: 1000 },
    adaptive: cfg.adaptive,
  });
  advance(100);
  expect(core.phase).toBe("download");
  advance(500);
  expect(core.phase).toBe("complete");
});

test("extending duration below the new coverage floor revokes confirmation", async () => {
  const core = new RunnerCore(new FakeBackend());
  const cfg = makeConfig({
    duration: { downloadMs: 1_000 },
    adaptive: {
      enabled: true,
      minCoverageRatio: 0.5,
      stabilityThreshold: 0.9,
      maxPhaseReductionRatio: 1,
      minTransferSamples: 4,
      confirmationMs: 200,
    },
  });
  await core.start(cfg);
  advance(600);
  for (let i = 0; i < 15; i++) core.ingestThroughput("down", 1_000, 100, 0.1);

  core.reconfigure({
    stages: cfg.stages,
    duration: { ...cfg.duration, downloadMs: 2_000 },
    adaptive: cfg.adaptive,
  });
  advance(cfg.adaptive.confirmationMs);

  expect(core.phase).toBe("download");
});

test("adaptive completion can be enabled after a stable stage has started", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const cfg = makeConfig({
    duration: { downloadMs: 2000 },
    adaptive: { enabled: false, minTransferSamples: 4 },
  });
  await core.start(cfg);
  advance(400);
  for (let i = 0; i < 10; i++) core.ingestThroughput("down", 1000, 100, 0.1);

  core.reconfigure({
    stages: cfg.stages,
    duration: cfg.duration,
    adaptive: { ...cfg.adaptive, enabled: true },
  });
  advance(cfg.adaptive.confirmationMs);

  expect(core.phase).toBe("complete");
});

test("shortening confirmation is re-evaluated immediately", async () => {
  const core = new RunnerCore(new FakeBackend());
  const cfg = makeConfig({
    duration: { downloadMs: 5_000 },
    adaptive: {
      enabled: true,
      minCoverageRatio: 0,
      stabilityThreshold: 0.9,
      maxPhaseReductionRatio: 1,
      minTransferSamples: 4,
      confirmationMs: 1_000,
    },
  });
  await core.start(cfg);
  advance(10);
  for (let i = 0; i < 15; i++) core.ingestThroughput("down", 1_000, 100, 0.1);
  advance(300);
  expect(core.phase).toBe("download");

  core.reconfigure({
    stages: cfg.stages,
    duration: cfg.duration,
    adaptive: { ...cfg.adaptive, confirmationMs: 200 },
  });

  expect(core.phase).toBe("complete");
});

test("adaptive completion can be disabled before a stable stage arms", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const cfg = makeConfig({
    duration: { downloadMs: 2000 },
    adaptive: { enabled: true, minTransferSamples: 4 },
  });
  await core.start(cfg);
  advance(400);
  for (let i = 0; i < 10; i++) core.ingestThroughput("down", 1000, 100, 0.1);

  core.reconfigure({
    stages: cfg.stages,
    duration: cfg.duration,
    adaptive: { ...cfg.adaptive, enabled: false },
  });
  advance(cfg.adaptive.confirmationMs * 2);

  expect(core.phase).toBe("download");
});

// ---------------------------------------------------------------------------
// Exact presentation and fixed-time stability from the same observations
// ---------------------------------------------------------------------------

test("raw samples reduce at source cadence while display events stay at 10 Hz", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((event) => events.push(event));
  await core.start(makeConfig());

  for (let i = 0; i < 50; i++) {
    fakeNow += 20;
    core.ingestThroughput("down", 1000, 20, 0.02);
  }
  expect(events.filter((event) => event.type === "throughput").length).toBe(10);

  advance(1000);
  const complete = events.find((event) => event.type === "complete");
  expect(
    complete?.type === "complete" && complete.result.download?.totalBytes,
  ).toBe(1000);
});

test("presentation derives from exact bytes and time, not backend instantaneous diagnostics", async () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  const events: RunnerEvent[] = [];
  core.on((e) => events.push(e));

  const cfg = makeConfig({ duration: { downloadMs: 100000 } });
  await core.start(cfg); // enters download at elapsed 0

  const DT = 200;
  const RAW = 1000;
  const N = 12;
  const DELTA = 50;

  // Backend diagnostics claim 1000 B/s, while exact observations carry
  // 50 bytes per 100 ms = 500 B/s. Presentation must use the latter.
  core.ingestThroughput("down", 0, 0, 0.1);

  for (let i = 0; i < N; i++) {
    fakeNow += DT;
    core.ingestThroughput("down", RAW, DELTA, 0.1);
  }

  const throughputSamples = events.filter(
    (e): e is Extract<RunnerEvent, { type: "throughput" }> =>
      e.type === "throughput",
  );
  const lastSample = throughputSamples.at(-1)!;

  expect(lastSample.sample.bytesPerSec).toBeCloseTo(500, 6);
  expect(lastSample.sample.bytesPerSec).not.toBe(RAW);

  // Raw byte totals are exact and untouched by either smoothing: N steps of
  // DELTA plus the seed sample's 0 bytes.
  expect(lastSample.sample.bytesCumulative).toBe(N * DELTA);

  // Final reduction uses the same exact byte/time evidence independently.
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

test("setBackgroundActivity reaches the backend so a hidden tab can park", () => {
  const backend = new FakeBackend();
  const core = new RunnerCore(backend);
  core.setBackgroundActivity(false);
  core.setBackgroundActivity(true);
  expect(backend.calls).toEqual(["background:false", "background:true"]);
});

test("setBackgroundActivity is optional on a backend that has no keepalive", () => {
  const backend = new FakeBackend();
  delete (backend as Partial<FakeBackend>).setBackgroundActivity;
  const core = new RunnerCore(backend);
  expect(() => core.setBackgroundActivity(false)).not.toThrow();
});
