import { test, expect, mock } from "bun:test";
import type { CoreHost } from "./core";
import type {
  RunnerConfig,
  RunnerEvent,
  FlowDirection,
  PhaseActivity,
  StallInfo,
  Phase,
} from "./contract";
import type { DummyOptions, DummySampleContext } from "./dummy";

// dummy.ts reads BUILD.clientVersion, which buildenv.ts fills in from Vite
// `define` tokens (__GM_*__) at bundle time. Those do not exist under plain
// `bun test`, so buildenv.ts throws on import unless mocked first, ahead of the
// dynamic import of dummy.ts below.
mock.module("../buildenv", () => ({
  BUILD: {
    defaultEngine: "dummy",
    allowDummy: true,
    devTools: true,
    buildLabel: "test",
    clientVersion: "0.0.0+test",
  },
}));
const { DummyBackend } = await import("./dummy");
type DummyBackendInstance = InstanceType<typeof DummyBackend>;

// A minimal but complete RunnerConfig fixture, mirroring schedule.test.ts's
// BASE_CONFIG: store.svelte.ts's DEFAULT_CONFIG cannot be imported outside
// Svelte's runtime.
const BASE_CONFIG: RunnerConfig = {
  stages: { latency: true, download: true, upload: true, bidirectional: false },
  skipLoadedLatencyWhenStageOff: true,
  duration: {
    warmupMs: 800,
    latencyMs: 4000,
    downloadMs: 10000,
    uploadMs: 10000,
    bidirectionalMs: 10000,
  },
  pingCadence: "reply-driven",
  loadedPingCadence: "medium",
  transferStreams: { mode: "auto", count: 6 },
  experimentalChunkedDownload: false,
  experimentalDatagramThroughput: false,
  transports: { throughputTarget: "current", latencyTarget: "auto" },
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
    minCoverageRatio: 0.52,
    stabilityThreshold: 0.86,
    maxPhaseReductionRatio: 0.5,
    minLatencySamples: 8,
    minTransferSamples: 12,
    glideMs: 1100,
  },
  visualization: { throughputMaxBytesPerSec: "auto" },
};

class MockHost implements CoreHost {
  #config: RunnerConfig | null;
  #phase: Phase = "idle";
  #elapsed = 0;

  throughput: {
    dir: FlowDirection;
    bytesPerSec: number;
    bytesDelta: number;
    durationSec: number;
  }[] = [];
  latency: { rttMs: number; underLoad: boolean; lost: boolean }[] = [];
  events: RunnerEvent[] = [];
  stalls: StallInfo[] = [];
  resumeCount = 0;

  constructor(config: RunnerConfig | null = BASE_CONFIG) {
    this.#config = config;
  }

  setPhase(p: Phase): void {
    this.#phase = p;
  }
  setElapsed(e: number): void {
    this.#elapsed = e;
  }

  get config(): RunnerConfig | null {
    return this.#config;
  }
  get phase(): Phase {
    return this.#phase;
  }
  get elapsed(): number {
    return this.#elapsed;
  }

  ingestThroughput(
    dir: FlowDirection,
    bytesPerSec: number,
    bytesDelta: number,
    durationSec: number,
  ): void {
    this.throughput.push({ dir, bytesPerSec, bytesDelta, durationSec });
  }
  ingestLatency(rttMs: number, underLoad: boolean, lost: boolean): void {
    this.latency.push({ rttMs, underLoad, lost });
  }
  stall(info: StallInfo): void {
    this.stalls.push(info);
  }
  resume(): void {
    this.resumeCount++;
  }
  reportTransport(): void {}
  emit(e: RunnerEvent): void {
    this.events.push(e);
  }
  fail(): void {}
  failStage(): void {}
}

const DOWNLOAD_ACTIVITY: PhaseActivity = {
  stage: "download",
  transfer: ["down"],
  loadedLatency: false,
};
const BIDI_ACTIVITY: PhaseActivity = {
  stage: "bidirectional",
  transfer: ["down", "up"],
  loadedLatency: false,
};
const LATENCY_ACTIVITY: PhaseActivity = {
  stage: "latency",
  transfer: [],
  loadedLatency: false,
};
const LOADED_DOWNLOAD_ACTIVITY: PhaseActivity = {
  stage: "download",
  transfer: ["down"],
  loadedLatency: true,
};

function makeBackend(opts: DummyOptions, host = new MockHost()) {
  const backend = new DummyBackend(opts);
  backend.attach(host);
  return { backend, host };
}

function tick(
  backend: DummyBackendInstance,
  overrides: Partial<DummySampleContext>,
): void {
  backend.sample({
    activity: DOWNLOAD_ACTIVITY,
    measuring: true,
    elapsed: 0,
    segStart: 0,
    segEnd: 10000,
    realNow: 0,
    ...overrides,
  });
}

function collectThroughput(
  backend: DummyBackendInstance,
  host: MockHost,
  activity: PhaseActivity,
  n: number,
): number[] {
  host.setPhase(activity.stage);
  for (let i = 0; i < n; i++) {
    const t = 2000 + i * 100;
    tick(backend, {
      activity,
      elapsed: t,
      segStart: 0,
      segEnd: 20000,
      realNow: t,
    });
  }
  return host.throughput.map((s) => s.bytesPerSec);
}

/** Drive `n` latency ticks, spaced past the ping cadence gate. */
function collectLatency(
  backend: DummyBackendInstance,
  host: MockHost,
  activity: PhaseActivity,
  n: number,
  segEnd = 20000,
): { rttMs: number; underLoad: boolean; lost: boolean }[] {
  host.setPhase(activity.stage);
  for (let i = 0; i < n; i++) {
    const t = 2000 + i * 90;
    tick(backend, { activity, elapsed: t, segStart: 0, segEnd, realNow: t });
  }
  return host.latency;
}

function mean(xs: number[]): number {
  return xs.reduce((a, b) => a + b, 0) / xs.length;
}
function relStd(xs: number[]): number {
  const m = mean(xs);
  const variance = mean(xs.map((x) => (x - m) ** 2));
  return Math.sqrt(variance) / m;
}

test("unloaded and loaded stages use their independent ping cadences", () => {
  const config = structuredClone(BASE_CONFIG);
  config.pingCadence = "fast";
  config.loadedPingCadence = "slow";
  const unloaded = makeBackend(
    { profile: "fiber", seed: 1 },
    new MockHost(config),
  );
  const loaded = makeBackend(
    { profile: "fiber", seed: 1 },
    new MockHost(config),
  );
  for (let t = 80; t <= 640; t += 80) {
    tick(unloaded.backend, {
      activity: LATENCY_ACTIVITY,
      realNow: t,
      elapsed: t,
    });
    tick(loaded.backend, {
      activity: LOADED_DOWNLOAD_ACTIVITY,
      realNow: t,
      elapsed: t,
    });
  }
  expect(unloaded.host.latency).toHaveLength(8);
  expect(loaded.host.latency).toHaveLength(1);
});

/* ================= Profile characteristics ================= */

test("fiber: high, steady throughput and low idle RTT", () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  const down = collectThroughput(backend, host, DOWNLOAD_ACTIVITY, 200);
  expect(mean(down)).toBeGreaterThan(100e6); // ~117.5 MB/s nominal
  expect(relStd(down)).toBeLessThan(0.1); // low jitter (0.04)

  const lat = collectLatency(backend, host, LATENCY_ACTIVITY, 60);
  expect(mean(lat.map((s) => s.rttMs))).toBeLessThan(10); // ~6ms idle
});

test("cable: high download, much lower upload, moderate idle RTT", () => {
  const { backend, host } = makeBackend({ profile: "cable", seed: 1 });
  const down = collectThroughput(backend, host, DOWNLOAD_ACTIVITY, 150);
  expect(mean(down)).toBeGreaterThan(25e6); // ~40 MB/s nominal

  const bidi = makeBackend({ profile: "cable", seed: 1 });
  collectThroughput(bidi.backend, bidi.host, BIDI_ACTIVITY, 150);
  const upSamples = bidi.host.throughput
    .filter((s) => s.dir === "up")
    .map((s) => s.bytesPerSec);
  expect(mean(upSamples)).toBeLessThan(mean(down) / 5); // ~2.75 MB/s vs ~40 MB/s
});

test("lte: modest, jittery throughput and higher idle RTT than wired profiles", () => {
  const { backend, host } = makeBackend({ profile: "lte", seed: 1 });
  const down = collectThroughput(backend, host, DOWNLOAD_ACTIVITY, 200);
  expect(mean(down)).toBeGreaterThan(4e6);
  expect(mean(down)).toBeLessThan(14e6); // ~8 MB/s nominal, well below cable/fiber

  const lat = collectLatency(backend, host, LATENCY_ACTIVITY, 60);
  expect(mean(lat.map((s) => s.rttMs))).toBeGreaterThan(20); // ~38ms idle
});

test("satellite: very high idle RTT dwarfing every other profile", () => {
  const { backend, host } = makeBackend({ profile: "satellite", seed: 1 });
  const lat = collectLatency(backend, host, LATENCY_ACTIVITY, 60);
  expect(mean(lat.map((s) => s.rttMs))).toBeGreaterThan(400); // ~600ms idle

  const fiber = makeBackend({ profile: "fiber", seed: 1 });
  const fiberLat = collectLatency(
    fiber.backend,
    fiber.host,
    LATENCY_ACTIVITY,
    60,
  );
  expect(mean(lat.map((s) => s.rttMs))).toBeGreaterThan(
    mean(fiberLat.map((s) => s.rttMs)) * 20,
  );
});

test("throttled: the lowest throughput of any profile", () => {
  const profiles: DummyOptions["profile"][] = [
    "fiber",
    "cable",
    "lte",
    "satellite",
    "throttled",
  ];
  const means = profiles.map((profile) => {
    const { backend, host } = makeBackend({ profile, seed: 1 });
    return mean(collectThroughput(backend, host, DOWNLOAD_ACTIVITY, 150));
  });
  const throttledMean = means[profiles.indexOf("throttled")];
  for (const [i, profile] of profiles.entries()) {
    if (profile === "throttled") continue;
    expect(throttledMean).toBeLessThan(means[i]);
  }
  expect(throttledMean).toBeLessThan(3e6); // ~1.19 MB/s nominal
});

test("idleHintMs reflects the active profile's idle RTT ordering", () => {
  const fiber = new DummyBackend({ profile: "fiber" });
  const satellite = new DummyBackend({ profile: "satellite" });
  expect(fiber.idleHintMs()).toBeLessThan(satellite.idleHintMs());
});

/* ================= Lifecycle contract ================= */

test("sampling during warmup pushes no samples", () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  host.setPhase("download");
  tick(backend, {
    activity: DOWNLOAD_ACTIVITY,
    measuring: false,
    elapsed: 5000,
    realNow: 5000,
  });
  expect(host.throughput).toHaveLength(0);
  expect(host.latency).toHaveLength(0);
});

test("measurement sampling pushes throughput samples", () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  host.setPhase("download");
  tick(backend, {
    activity: DOWNLOAD_ACTIVITY,
    measuring: true,
    elapsed: 2000,
    realNow: 2000,
  });
  expect(host.throughput.length).toBeGreaterThan(0);
});

test("a latency-only activity (no transfer lanes) never produces throughput samples", () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  host.setPhase("latency");
  for (let i = 0; i < 30; i++) {
    tick(backend, {
      activity: LATENCY_ACTIVITY,
      elapsed: 2000 + i * 90,
      realNow: 2000 + i * 90,
    });
  }
  expect(host.throughput).toHaveLength(0);
  expect(host.latency.length).toBeGreaterThan(0);
});

test("stage lifecycle starts and stops scheduled samples", async () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  host.setPhase("download");
  host.setElapsed(2000);
  backend.onStageBegin(DOWNLOAD_ACTIVITY);
  backend.onStageMeasure(DOWNLOAD_ACTIVITY);
  await Bun.sleep(20);
  expect(host.throughput.length).toBeGreaterThan(0);

  backend.onStageEnd(DOWNLOAD_ACTIVITY);
  const stopped = host.throughput.length;
  await Bun.sleep(120);
  expect(host.throughput).toHaveLength(stopped);
});

test("onRunStart resets the throughput/ping cadence gates so the next tick fires immediately", () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  host.setPhase("download");
  // First tick seeds the cadence gate at realNow=1000.
  tick(backend, { activity: DOWNLOAD_ACTIVITY, elapsed: 2000, realNow: 1000 });
  expect(host.throughput.length).toBeGreaterThan(0);
  const firstCount = host.throughput.length;

  tick(backend, { activity: DOWNLOAD_ACTIVITY, elapsed: 2001, realNow: 1001 });
  expect(host.throughput.length).toBe(firstCount);

  backend.onRunStart(BASE_CONFIG);
  tick(backend, { activity: DOWNLOAD_ACTIVITY, elapsed: 2002, realNow: 1002 });
  expect(host.throughput.length).toBeGreaterThan(firstCount);
});

/* ================= Construction-time anomalies ================= */

test("throughputDipAt: a 40% dip lands inside the declared window and clears outside it", () => {
  const { backend, host } = makeBackend({
    profile: "fiber",
    seed: 1,
    anomalies: { throughputDipAt: [0.5] },
  });
  host.setPhase("download");
  const segEnd = 20000;
  const center = 0.5 * segEnd;

  tick(backend, {
    activity: DOWNLOAD_ACTIVITY,
    elapsed: center + 100,
    segStart: 0,
    segEnd,
    realNow: 100,
  });
  const dipped = host.throughput.at(-1)!.bytesPerSec;

  tick(backend, {
    activity: DOWNLOAD_ACTIVITY,
    elapsed: center + 2000,
    segStart: 0,
    segEnd,
    realNow: 3000,
  });
  const clear = host.throughput.at(-1)!.bytesPerSec;

  expect(dipped).toBeLessThan(clear * 0.75); // dip cuts throughput by 40%
});

test("latencySpikeAt: RTT roughly triples right at the declared fraction", () => {
  const { backend, host } = makeBackend({
    profile: "fiber",
    seed: 1,
    anomalies: { latencySpikeAt: [0.5] },
  });
  host.setPhase("latency");
  const segEnd = 20000;
  const center = 0.5 * segEnd;

  tick(backend, {
    activity: LATENCY_ACTIVITY,
    elapsed: center,
    segStart: 0,
    segEnd,
    realNow: 100,
  });
  const spiked = host.latency.at(-1)!.rttMs;

  tick(backend, {
    activity: LATENCY_ACTIVITY,
    elapsed: center + 5000,
    segStart: 0,
    segEnd,
    realNow: 5000,
  });
  const clear = host.latency.at(-1)!.rttMs;

  expect(spiked).toBeGreaterThan(clear * 2); // ~3x, allow for gaussian jitter
});

test("packetDropAt: loss probability jumps to 60% in the declared window", () => {
  const { backend, host } = makeBackend({
    profile: "fiber", // lossBase 0.0, so any loss must come from the anomaly
    seed: 1,
    anomalies: { packetDropAt: [0.5] },
  });
  host.setPhase("latency");
  const segEnd = 100000;
  const center = 0.5 * segEnd;

  // Sample many pings within the +/-3% burst window around the fraction.
  let lostCount = 0;
  let n = 0;
  for (let i = 0; i < 200; i++) {
    const t = center + i - 100; // spans the window
    tick(backend, {
      activity: LATENCY_ACTIVITY,
      elapsed: t,
      segStart: 0,
      segEnd,
      realNow: 100 + i * 90,
    });
    const last = host.latency.at(-1);
    if (last && Math.abs((t - center) / segEnd) < 0.03) {
      lostCount += last.lost ? 1 : 0;
      n++;
    }
  }
  expect(n).toBeGreaterThan(0);
  expect(lostCount / n).toBeGreaterThan(0.3); // well above the 0 baseline
});

/* ================= Live anomaly injection ================= */

test("injectAnomaly latency-spike: scales rtt by the default 3x within its window, then clears", () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  host.setPhase("download"); // any non-idle phase; injectAnomaly needs a running host
  host.setElapsed(1000);

  // Baseline RTT ahead of the spike.
  tick(backend, { activity: LATENCY_ACTIVITY, elapsed: 1000, realNow: 1000 });
  const baseline = host.latency.at(-1)!.rttMs;

  backend.injectAnomaly!({ kind: "latency-spike" });
  tick(backend, { activity: LATENCY_ACTIVITY, elapsed: 1200, realNow: 1200 });
  const spiked = host.latency.at(-1)!.rttMs;
  expect(spiked).toBeGreaterThan(baseline * 2);

  // Past the default 600ms window it should be back to baseline-ish.
  tick(backend, { activity: LATENCY_ACTIVITY, elapsed: 1601, realNow: 1601 });
  const cleared = host.latency.at(-1)!.rttMs;
  expect(cleared).toBeLessThan(spiked / 2);
});

test("injectAnomaly packet-loss: raises loss probability to the default 60% within its window", () => {
  const { backend, host } = makeBackend({
    profile: "fiber", // lossBase 0.0
    seed: 1,
  });
  host.setPhase("download");
  host.setElapsed(1000);
  // Widen the window (magnitude stays default 0.6) so enough pings land inside
  // it to estimate a probability: the fast unloaded cadence samples every 80ms,
  // and the default 900ms window barely fits ~11.
  backend.injectAnomaly!({ kind: "packet-loss", durationMs: 20000 });

  for (let i = 0; i < 150; i++) {
    const t = 1000 + i * 80; // matches the fast ping cadence exactly
    tick(backend, { activity: LATENCY_ACTIVITY, elapsed: t, realNow: t });
  }
  const lost = host.latency.filter((s) => s.lost).length;
  expect(host.latency.length).toBe(150);
  expect(lost / host.latency.length).toBeGreaterThan(0.4); // ~0.6 expected, generous band
  expect(lost / host.latency.length).toBeLessThan(0.8);
});

test("injectAnomaly throughput-drop: cuts bytesPerSec by the default 40% within its window", () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  host.setPhase("download");
  host.setElapsed(3000);

  tick(backend, { activity: DOWNLOAD_ACTIVITY, elapsed: 3000, realNow: 3000 });
  const baseline = host.throughput.at(-1)!.bytesPerSec;

  backend.injectAnomaly!({ kind: "throughput-drop" });
  tick(backend, { activity: DOWNLOAD_ACTIVITY, elapsed: 3200, realNow: 3200 });
  const dropped = host.throughput.at(-1)!.bytesPerSec;
  expect(dropped).toBeLessThan(baseline * 0.75); // ~40% cut, allow jitter

  // Past the default 600ms window, back to baseline-ish.
  tick(backend, { activity: DOWNLOAD_ACTIVITY, elapsed: 3700, realNow: 3700 });
  const recovered = host.throughput.at(-1)!.bytesPerSec;
  expect(recovered).toBeGreaterThan(dropped * 1.3);
});

test("injectAnomaly connection-drop: records dead air until the window lifts", () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  host.setPhase("download");
  host.setElapsed(1000);

  backend.injectAnomaly!({ kind: "connection-drop", durationMs: 500 });
  expect(host.stalls).toHaveLength(1);
  expect(host.stalls[0].reason).toBe("connection-lost");

  const justAfterInject = performance.now();
  // Still inside the drop window: a zero-byte interval, no resume.
  tick(backend, {
    activity: DOWNLOAD_ACTIVITY,
    elapsed: 1000,
    realNow: justAfterInject,
  });
  expect(host.throughput).toHaveLength(1);
  expect(host.throughput[0].bytesDelta).toBe(0);
  expect(host.resumeCount).toBe(0);

  // Past the drop window (real wall-clock, not virtual `elapsed`): resumes,
  // and the cadence gates reset so a later tick can synthesize again.
  const pastDrop = justAfterInject + 600;
  tick(backend, {
    activity: DOWNLOAD_ACTIVITY,
    elapsed: 1000,
    realNow: pastDrop,
  });
  expect(host.resumeCount).toBe(1);

  tick(backend, {
    activity: DOWNLOAD_ACTIVITY,
    elapsed: 1100,
    realNow: pastDrop + 100,
  });
  expect(host.throughput.length).toBeGreaterThan(0);
});

test("injectAnomaly is a no-op while idle (no config, or phase idle)", () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  // host.phase defaults to "idle".
  backend.injectAnomaly!({ kind: "latency-spike" });
  expect(host.stalls).toHaveLength(0);

  // Confirm no live anomaly is queued: a subsequent latency tick, even with the
  // phase flipped manually, shows no spike.
  host.setPhase("download");
  host.setElapsed(0);
  tick(backend, { activity: LATENCY_ACTIVITY, elapsed: 100, realNow: 100 });
  const rttNoSpike = host.latency.at(-1)!.rttMs;
  expect(rttNoSpike).toBeLessThan(20); // fiber idle RTT ~6ms, nowhere near a 3x spike
});

/* ================= Anomaly windows expire ================= */

test("a live anomaly is pruned once elapsed passes its window end", () => {
  const { backend, host } = makeBackend({ profile: "fiber", seed: 1 });
  host.setPhase("download");
  host.setElapsed(0);
  backend.injectAnomaly!({ kind: "throughput-drop", durationMs: 300 });

  tick(backend, { activity: DOWNLOAD_ACTIVITY, elapsed: 100, realNow: 100 });
  const inWindow = host.throughput.at(-1)!.bytesPerSec;

  // Well past the window: dropping the anomaly, throughput recovers.
  tick(backend, { activity: DOWNLOAD_ACTIVITY, elapsed: 5000, realNow: 5000 });
  const afterWindow = host.throughput.at(-1)!.bytesPerSec;

  expect(afterWindow).toBeGreaterThan(inWindow * 1.3);
});

/* ================= probe() ================= */

test("probe: emits pre-test idle pings and reports the profile's idle RTT + protocol", async () => {
  const { backend, host } = makeBackend({ profile: "satellite", seed: 1 });
  const info = await backend.probe(BASE_CONFIG);

  expect(info.preTestPingMs).toBeCloseTo(600, -1); // satellite idleRttMs
  expect(info.protocolNegotiated).toBe("http/1.1");
  expect(info.clientIp).toMatch(/^\d{1,3}(\.\d{1,3}){3}$/);
  expect(info.server.name).toContain("Graphite Edge");

  const pings = host.events.filter((e) => e.type === "latency");
  expect(pings).toHaveLength(4);
  for (const p of pings) {
    if (p.type !== "latency") continue;
    expect(p.sample.phase).toBe("idle");
    expect(p.sample.t).toBeLessThan(0); // pre-test pings carry negative t
    expect(p.sample.underLoad).toBe(false);
  }
}, 2000);

/* ================= describe() ================= */

test("describe: static engine identity and capability surface", () => {
  const { backend } = makeBackend({ profile: "fiber", seed: 1 });
  const info = backend.describe();
  expect(info.name).toBe("dummy");
  expect(info.latencyTransports).toContain("webtransport");
  expect(info.latencyTransports).toContain("websocket");
  expect(info.throughputTransports).toContain("webtransport");
  expect(info.throughputTransports).toContain("fetch-stream");
  expect(info.throughputTransports).not.toContain("websocket"); // never a byte-transfer lane
});
