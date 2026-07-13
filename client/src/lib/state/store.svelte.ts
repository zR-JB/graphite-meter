// App-wide reactive state for runner events, persisted settings, derived UI
// metrics, stage guards, compensation, scale, and latency lanes.
import type {
  RunnerEvent,
  Phase,
  ConnectivityState,
  InfraInfo,
  EngineInfo,
  RunResult,
  RunnerConfig,
  RunnerError,
  ThroughputSample,
  LatencySample,
  StabilitySnapshot,
  ThroughputResult,
  LatencyResult,
  StallInfo,
  TransportAttempt,
  TransportRole,
  StageFailure,
} from "../runner/contract";
import {
  estimateLiveCompensation,
  estimateResultCompensation,
  type CompensationEstimate,
} from "../compensation";
import {
  quantile,
  sharedThroughputScale,
  rateScaleIndex,
  rateUnit,
  rateValueAt,
  rawRateFrom,
} from "../format";
import { buildSegments } from "../runner/schedule";
import {
  canDisableBidirectional as canDisableBidirectionalPure,
  latestOneWayThroughputForPhase,
  latestBidirectionalLanes,
} from "./stageGuards";
import {
  loadPersisted,
  savePersisted,
  systemThemeDefault,
  type ThemePref,
  type SettingsTab,
} from "./persistence";

const SCALE_DWELL_MS = 700;

export const MEASURED_STAGES = ["latency", "download", "upload"] as const;
export type StageKey = (typeof MEASURED_STAGES)[number];
const TRANSFER_STAGES = ["download", "upload", "bidirectional"] as const;
const TERMINAL_PHASES: readonly Phase[] = [
  "idle",
  "complete",
  "aborted",
  "error",
];
const CONNECTION_FAILURE_REASONS: readonly RunnerError["reason"][] = [
  "connection-lost",
  "timeout",
  "preflight-failed",
  "transport-unavailable",
];

export interface LatencyLane {
  key: StageKey;
  min: number | null;
  max: number | null;
  p10: number | null;
  p90: number | null;
  average: number | null;
  current: number | null;
  jitter: number | null;
  lossRatio: number;
  count: number;
  active: boolean;
}

export const DEFAULT_CONFIG: RunnerConfig = {
  stages: { latency: true, download: true, upload: true, bidirectional: false },
  skipLoadedLatencyWhenStageOff: true,
  duration: {
    warmupMs: 800,
    latencyMs: 4000,
    downloadMs: 10000,
    uploadMs: 10000,
    bidirectionalMs: 10000,
  },
  pingConcurrency: "medium",
  parallelStreams: 6,
  experimentalChunkedDownload: false,
  endpoint: { host: "auto", port: 443 },
  compensation: {
    profile: "lan",
    transport: "auto",
    params: {
      mtuBytes: 1500,
      ipVersion: "auto",
      vlanTagged: false,
      tcpOptionsMinBytes: 0,
      tcpOptionsMaxBytes: 12,
      encapsulationBytes: 0,
      quicConnIdMinBytes: 0,
      quicConnIdMaxBytes: 20,
    },
  },
  adaptive: {
    enabled: true,
    minCoverageRatio: 0.52,
    stabilityThreshold: 0.86,
    maxPhaseReductionRatio: 0.5,
    minLatencySamples: 8,
    minTransferSamples: 12,
    glideMs: 1100,
  },
  visualization: { throughputMaxBytesPerSec: "auto" },
};

export const DURATION_PRESETS = {
  short: {
    warmupMs: 600,
    latencyMs: 2500,
    downloadMs: 5000,
    uploadMs: 5000,
    bidirectionalMs: 5000,
  },
  medium: {
    warmupMs: 800,
    latencyMs: 4000,
    downloadMs: 10000,
    uploadMs: 10000,
    bidirectionalMs: 10000,
  },
  long: {
    warmupMs: 1200,
    latencyMs: 6000,
    downloadMs: 20000,
    uploadMs: 20000,
    bidirectionalMs: 20000,
  },
} as const;

const MAX_SAMPLES = 1200;
const MAX_IDLE_SAMPLES = 60;

const UNIT_STEP_UP_HEADROOM = 1.2;

class AppStore {
  throughput = $state<ThroughputSample[]>([]);
  latency = $state<LatencySample[]>([]);
  idleLatency = $state<LatencySample[]>([]);

  phase = $state<Phase>("idle");
  phaseStage = $state<TransportRole | null>(null);
  phaseStartedAtMs = $state(0);
  phaseFraction = $state(0);
  phaseElapsedMs = $state(0);
  phaseBudgetMs = $state(0);
  measuring = $state(true);
  stalledSince = $state(0);
  stallInfo = $state<StallInfo | null>(null);
  currentTransport = $state<TransportAttempt | null>(null);
  transportLog = $state<TransportAttempt[]>([]);
  liveStability = $state<{
    latency: StabilitySnapshot | null;
    download: StabilitySnapshot | null;
    upload: StabilitySnapshot | null;
  }>({ latency: null, download: null, upload: null });
  runSeq = $state(0);

  connectivity = $state<ConnectivityState>("connected");
  infra = $state<InfraInfo | null>(null);
  engineInfo = $state<EngineInfo | null>(null);
  result = $state<RunResult | null>(null);
  stageResults = $state<{
    download: ThroughputResult | null;
    upload: ThroughputResult | null;
    latency: LatencyResult | null;
  }>({ download: null, upload: null, latency: null });
  error = $state<RunnerError | null>(null);
  stageFailures = $state<Partial<Record<TransportRole, StageFailure>>>({});
  startEpoch = $state(0);

  config = $state<RunnerConfig>(structuredClone(DEFAULT_CONFIG));
  unitBase = $state<"base10" | "base2">("base10");
  unitKind = $state<"bits" | "bytes">("bits");
  theme = $state<ThemePref>("dark");
  showWireEstimates = $state(false);
  dockWidth = $state<{ left: number; right: number }>({
    left: 400,
    right: 400,
  });
  settingsTab = $state<SettingsTab>("setup");
  debugLogging = $state(false);

  constructor() {
    const p = loadPersisted();
    this.config = p.config;
    this.unitBase = p.unitBase;
    this.unitKind = p.unitKind;
    this.theme = p.theme;
    this.showWireEstimates = p.showWireEstimates;
    this.dockWidth = p.dockWidth;
    this.settingsTab = p.settingsTab === "developer" ? "developer" : "setup";
    this.debugLogging = p.debugLogging;
  }

  liveMetric = $derived.by(() => {
    return {
      value: this.toUnit(this.liveTransferBytesPerSec),
      unit: this.unitLabel,
    };
  });

  // End-of-run headline is phase-agnostic: latency-only, upload-only, and
  // bidirectional-only runs all resolve to the metric that actually ran.
  finalMetric = $derived.by<
    | { kind: "speed"; bytesPerSec: number }
    | { kind: "latency"; ms: number }
    | null
  >(() => {
    const r = this.stageResults;
    const bidi = this.result?.bidirectional;
    if (bidi)
      return {
        kind: "speed",
        bytesPerSec:
          bidi.down.reportedBytesPerSec + bidi.up.reportedBytesPerSec,
      };
    if (r.upload)
      return { kind: "speed", bytesPerSec: r.upload.reportedBytesPerSec };
    if (r.download)
      return { kind: "speed", bytesPerSec: r.download.reportedBytesPerSec };
    if (r.latency) return { kind: "latency", ms: r.latency.reportedMs };
    return null;
  });

  liveTransferBytesPerSec = $derived.by(() => {
    // Bidirectional is displayed as aggregate throughput: latest down + latest up.
    if (this.phase === "download" || this.phase === "upload") {
      return latestOneWayThroughputForPhase(this.phase, this.throughput);
    }
    if (this.phase === "bidirectional") {
      const { down, up } = latestBidirectionalLanes(this.throughput);
      return down + up;
    }
    return 0;
  });

  liveBidirectional = $derived.by<{ down: number; up: number } | null>(() => {
    if (this.phase !== "bidirectional") return null;
    return latestBidirectionalLanes(this.throughput);
  });

  pulseLatency = $derived.by<LatencySample[]>(() => {
    // While idle, the pulse reads the keepalive lane if available. During a run,
    // it reads measured samples so loss/jitter reflect the active test.
    if (this.isRunning) return this.latency;
    return this.idleLatency.length ? this.idleLatency : this.latency;
  });

  liveRtt = $derived(
    this.pulseLatency.length
      ? this.pulseLatency.at(-1)!.rttMs
      : (this.infra?.preTestPingMs ?? 0),
  );

  rollingLossPct = $derived.by(() => {
    const w = this.pulseLatency.slice(-20);
    if (!w.length) return 0;
    return (w.filter((s) => s.lost).length / w.length) * 100;
  });

  jitterMs = $derived.by(() => {
    const w = this.pulseLatency.slice(-30).filter((s) => !s.lost);
    if (w.length < 2) return 0;
    let acc = 0;
    for (let i = 1; i < w.length; i++)
      acc += Math.abs(w[i].rttMs - w[i - 1].rttMs);
    return acc / (w.length - 1);
  });

  effectiveConnectivity = $derived.by<ConnectivityState>(() => {
    // Do not pin the error phase offline forever: connection failures latch
    // connectivity once, then the restarted keepalive can report recovery.
    if (this.isRunning && !this.measuring) return "offline";
    if (this.connectivity === "offline") return "offline";
    if (this.rollingLossPct > 5) return "unstable";
    if (this.rollingLossPct > 0.5 || this.jitterMs > 30) return "degraded";
    return "connected";
  });

  totalEtaMs = $derived(buildSegments(this.config).totalMs);

  phaseRemainingMs = $derived(
    Math.max(0, this.phaseBudgetMs - this.phaseElapsedMs),
  );

  bytesTransferred = $derived(this.throughput.at(-1)?.bytesCumulative ?? 0);

  isRunning = $derived(!TERMINAL_PHASES.includes(this.phase));

  transferFailures = $derived.by<StageFailure[]>(() => {
    return TRANSFER_STAGES.flatMap((stage) => this.stageFailures[stage] ?? []);
  });

  activeStages = $derived.by<StageKey[]>(() => {
    return MEASURED_STAGES.filter((stage) => this.config.stages[stage]);
  });

  // Mid-run toggles may only affect future stages. The current stage is already
  // wired and past stages have produced results.
  canToggleStage(stage: StageKey): boolean {
    if (!this.isRunning) return true;
    const currentIndex = MEASURED_STAGES.indexOf(this.phase as StageKey);
    const stageIndex = MEASURED_STAGES.indexOf(stage);
    return currentIndex >= 0 && stageIndex > currentIndex;
  }

  toggleStage(stage: StageKey): boolean {
    if (!this.canToggleStage(stage)) return false;

    const currentlyEnabled = this.config.stages[stage];
    const enabledCount = this.activeStages.length;

    if (currentlyEnabled && enabledCount <= 1) return false;

    this.config.stages[stage] = !currentlyEnabled;
    return true;
  }

  canDisableBidirectional(): boolean {
    // Bidirectional is outside the "at least one stage" set, so its stage-track
    // off rule lives separately from canToggleStage().
    return canDisableBidirectionalPure(this.phase, this.isRunning);
  }

  disableBidirectional(): boolean {
    if (!this.config.stages.bidirectional) return false;
    if (!this.canDisableBidirectional()) return false;
    this.config.stages.bidirectional = false;
    return true;
  }

  latencyEnabled = $derived(
    this.config.stages.latency || !this.config.skipLoadedLatencyWhenStageOff,
  );

  // The store remains bytes/sec-native. UI conversion happens at the edge via
  // toUnit(), which keeps compensation and scale math in one raw domain.
  liveCompensation = $derived<CompensationEstimate>(
    estimateLiveCompensation(
      this.liveTransferBytesPerSec,
      this.config.compensation,
      this.phase === "upload" ? "upload" : "download",
      this.infra?.firstHopProtocol,
      this.infra?.firstHopSecure,
      this.infra?.clientIpVersion,
    ),
  );

  downloadCompensation = $derived<CompensationEstimate>(
    estimateResultCompensation(
      this.stageResults.download,
      "download",
      this.config.compensation,
      this.infra?.firstHopProtocol,
      this.infra?.firstHopSecure,
      this.infra?.clientIpVersion,
    ),
  );

  uploadCompensation = $derived<CompensationEstimate>(
    estimateResultCompensation(
      this.stageResults.upload,
      "upload",
      this.config.compensation,
      this.infra?.firstHopProtocol,
      this.infra?.firstHopSecure,
      this.infra?.clientIpVersion,
    ),
  );

  #peakBytesPerSec = $derived.by(() => {
    let peak = 0;
    for (const s of this.throughput)
      if (s.bytesPerSec > peak) peak = s.bytesPerSec;
    return peak;
  });

  #sustainedPeakBytesPerSec = $derived.by(() => {
    // Scale from a time-weighted sustained peak, not a single transient spike.
    const arr = this.throughput;
    const n = arr.length;
    if (n === 0) return 0;
    if (n === 1) return arr[0].bytesPerSec;
    const weighted = new Array<{ v: number; w: number }>(n);
    for (let i = 0; i < n; i++) {
      const w = i === 0 ? arr[1].t - arr[0].t : arr[i].t - arr[i - 1].t;
      weighted[i] = { v: arr[i].bytesPerSec, w: Math.max(1, w) };
    }
    weighted.sort((a, b) => b.v - a.v);
    let acc = 0;
    for (const s of weighted) {
      acc += s.w;
      if (acc >= SCALE_DWELL_MS) return s.v;
    }
    return weighted[n - 1].v;
  });

  displayScaleBytesPerSec = $derived.by(() => {
    const cfg = this.config.visualization.throughputMaxBytesPerSec;
    if (typeof cfg === "number" && cfg > 0) return cfg;
    return sharedThroughputScale(this.#sustainedPeakBytesPerSec);
  });

  #unitIndex = $derived.by(() => {
    // Unit prefix follows the observed raw peak, not the rounded-up dial ceiling,
    // so readings do not flip to "0.xx" just because scale headroom increased.
    const cfg = this.config.visualization.throughputMaxBytesPerSec;
    const refBytesPerSec =
      typeof cfg === "number" && cfg > 0 ? cfg : this.#peakBytesPerSec;
    const baseUnits =
      this.unitKind === "bytes" ? refBytesPerSec : refBytesPerSec * 8;
    return rateScaleIndex(baseUnits, this.unitBase, UNIT_STEP_UP_HEADROOM);
  });

  get unitLabel() {
    return rateUnit(this.unitBase, this.unitKind, this.#unitIndex);
  }

  toUnit(bytesPerSec: number): number {
    return rateValueAt(
      bytesPerSec,
      this.unitBase,
      this.unitKind,
      this.#unitIndex,
    );
  }

  fromUnit(displayValue: number): number {
    return rawRateFrom(
      displayValue,
      this.unitBase,
      this.unitKind,
      this.#unitIndex,
    );
  }

  ingest = (e: RunnerEvent) => {
    switch (e.type) {
      case "infra":
        this.infra = e.info;
        break;
      case "phase": {
        this.phase = e.transition.to;
        this.phaseStage = e.transition.stage;
        this.phaseStartedAtMs = e.transition.t;
        this.phaseFraction = 0;
        // Stamp the wall-clock run start once, not on every warmup segment.
        if (e.transition.from === "idle") this.startEpoch = Date.now();
        break;
      }
      case "progress":
        this.phaseFraction = e.fraction;
        this.phaseElapsedMs = e.phaseElapsedMs;
        this.phaseBudgetMs = e.phaseBudgetMs;
        this.measuring = e.measuring;
        break;
      case "stall":
        // Store only the presentation latch; measurement logic stays in the core.
        this.measuring = false;
        this.stalledSince = performance.now();
        this.stallInfo = e.info;
        break;
      case "resume":
        this.#clearStall();
        break;
      case "transport":
        this.currentTransport = e.attempt;
        this.transportLog.push(e.attempt);
        break;
      case "stageSkipped":
        this.stageFailures = {
          ...this.stageFailures,
          [e.failure.stage]: e.failure,
        };
        break;
      case "stability":
        this.liveStability[e.snapshot.phase] = e.snapshot;
        break;
      case "stageResult":
        if (e.stage === "latency") this.stageResults.latency = e.result;
        else this.stageResults[e.stage] = e.result;
        break;
      case "throughput":
        this.throughput.push(e.sample);
        if (this.throughput.length > MAX_SAMPLES) this.throughput.shift();
        break;
      case "latency":
        if (e.sample.phase === "idle") {
          this.idleLatency.push(e.sample);
          if (this.idleLatency.length > MAX_IDLE_SAMPLES)
            this.idleLatency.shift();
        } else {
          this.latency.push(e.sample);
          if (this.latency.length > MAX_SAMPLES) this.latency.shift();
        }
        break;
      case "connectivity":
        this.connectivity = e.state;
        break;
      case "complete":
        this.result = e.result;
        this.phase = "complete";
        break;
      case "error": {
        this.error = e.error;
        // A terminal error resolves any in-flight stall, so the idle/error view
        // is not stuck in "measuring=false".
        this.#clearStall();
        if (CONNECTION_FAILURE_REASONS.includes(e.error.reason)) {
          this.connectivity = "offline";
        }
        const p = e.error.partial;
        if (p) {
          if (p.download) this.stageResults.download = p.download;
          if (p.upload) this.stageResults.upload = p.upload;
          if (p.latency) this.stageResults.latency = p.latency;
        }
        this.phase = "error";
        break;
      }
    }
  };

  #clearStall() {
    this.measuring = true;
    this.stalledSince = 0;
    this.stallInfo = null;
  }

  reset() {
    this.throughput = [];
    this.latency = [];
    this.phase = "idle";
    this.phaseStage = null;
    this.phaseStartedAtMs = 0;
    this.phaseFraction = 0;
    this.phaseElapsedMs = 0;
    this.phaseBudgetMs = 0;
    this.#clearStall();
    this.currentTransport = null;
    this.transportLog = [];
    this.liveStability = { latency: null, download: null, upload: null };
    this.stageResults = { download: null, upload: null, latency: null };
    this.stageFailures = {};
    this.result = null;
    this.error = null;
    this.startEpoch = 0;
    this.runSeq++;
  }

  latencyLanes = $derived.by<LatencyLane[]>(() => {
    return MEASURED_STAGES.map((key) => {
      // Bucket by the sample's stamped phase. Pre-test pings are phase "idle",
      // so they never contaminate the measured latency lane.
      const laneSamples = this.latency.filter((s) =>
        key === "latency"
          ? s.phase === "latency"
          : s.underLoad && s.phase === key,
      );
      const valid = laneSamples.filter((s) => !s.lost);
      const sorted = valid.map((s) => s.rttMs).sort((a, b) => a - b);
      const avg = valid.length
        ? valid.reduce((sum, s) => sum + s.rttMs, 0) / valid.length
        : null;
      const jitter =
        avg != null && valid.length >= 2
          ? valid.reduce((sum, s) => sum + Math.abs(s.rttMs - avg), 0) /
            valid.length
          : null;
      const lossRatio = laneSamples.length
        ? laneSamples.filter((s) => s.lost).length / laneSamples.length
        : 0;
      return {
        key,
        min: sorted.at(0) ?? null,
        max: sorted.at(-1) ?? null,
        p10: quantile(sorted, 0.1),
        p90: quantile(sorted, 0.9),
        average: avg,
        current: valid.at(-1)?.rttMs ?? null,
        jitter,
        lossRatio,
        count: laneSamples.length,
        active: this.phase === key,
      };
    });
  });
}

export const store = new AppStore();

const SAVE_DEBOUNCE_MS = 250;

if (typeof window !== "undefined") {
  // The inline boot script sets data-theme before paint; this effect keeps it
  // synced afterward, including live OS changes when theme is "auto".
  let systemPrefersLight = $state(systemThemeDefault() === "light");
  if (window.matchMedia) {
    window
      .matchMedia("(prefers-color-scheme: light)")
      .addEventListener("change", (e) => {
        systemPrefersLight = e.matches;
      });
  }

  $effect.root(() => {
    $effect(() => {
      const resolved =
        store.theme === "auto"
          ? systemPrefersLight
            ? "light"
            : "dark"
          : store.theme;
      document.documentElement.setAttribute("data-theme", resolved);
    });

    let timer: ReturnType<typeof setTimeout> | undefined;
    $effect(() => {
      // Touch every persisted field so any preference/config change schedules
      // one debounced write of a plain snapshot.
      const snapshot = {
        config: $state.snapshot(store.config),
        unitBase: store.unitBase,
        unitKind: store.unitKind,
        theme: store.theme,
        showWireEstimates: store.showWireEstimates,
        dockWidth: $state.snapshot(store.dockWidth),
        settingsTab: store.settingsTab,
        debugLogging: store.debugLogging,
      };
      clearTimeout(timer);
      timer = setTimeout(() => savePersisted(snapshot), SAVE_DEBOUNCE_MS);
      return () => clearTimeout(timer);
    });
  });
}
