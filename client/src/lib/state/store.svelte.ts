// App-wide reactive state for runner events, persisted settings, derived UI
// metrics, stage guards, compensation, scale, and latency lanes.
import type {
  RunnerEvent,
  Phase,
  ConnectivityState,
  InfraInfo,
  TransportDiscovery,
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
  TransportRole,
  ConnectionRole,
  StageFailure,
} from "../runner/contract";
import {
  CONNECTION_FAILURE_REASONS,
  presentConnections,
  type ConnectionPresentation,
  type ConnectionValidation,
} from "../runner/connectionModel";
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
import { DEFAULT_CONFIG } from "./defaults";
import {
  loadPersisted,
  savePersisted,
  systemThemeDefault,
  type ThemePref,
  type SettingsTab,
} from "./persistence";

const SCALE_DWELL_MS = 700;

const MEASURED_STAGES = ["latency", "download", "upload"] as const;
export type StageKey = (typeof MEASURED_STAGES)[number];
const TRANSFER_STAGES = ["download", "upload", "bidirectional"] as const;
const TERMINAL_PHASES: readonly Phase[] = [
  "idle",
  "complete",
  "aborted",
  "error",
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
  liveStability = $state<{
    latency: StabilitySnapshot | null;
    download: StabilitySnapshot | null;
    upload: StabilitySnapshot | null;
  }>({ latency: null, download: null, upload: null });
  runSeq = $state(0);

  connectivity = $state<ConnectivityState>("connected");
  infra = $state<InfraInfo | null>(null);
  transportDiscovery = $state<TransportDiscovery | null>(null);
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
  activeConfig = $state<RunnerConfig | null>(null);
  activeConnections = $state<Record<
    ConnectionRole,
    ConnectionPresentation
  > | null>(null);
  connectionValidation = $state<ConnectionValidation>({
    throughput: { selection: "current", state: "stale" },
    latency: { selection: "auto", state: "stale" },
  });
  connections = $derived(
    presentConnections(
      this.config,
      this.transportDiscovery,
      this.connectionValidation,
      this.infra,
    ),
  );
  runConfig = $derived(this.activeConfig ?? this.config);
  runConnections = $derived(this.activeConnections ?? this.connections);
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
    const persisted = loadPersisted();
    this.config = persisted.config;
    this.unitBase = persisted.unitBase;
    this.unitKind = persisted.unitKind;
    this.theme = persisted.theme;
    this.showWireEstimates = persisted.showWireEstimates;
    this.dockWidth = persisted.dockWidth;
    this.settingsTab = persisted.settingsTab;
    this.debugLogging = persisted.debugLogging;
  }

  // End-of-run headline is phase-agnostic: latency-only, upload-only, and
  // bidirectional-only runs all resolve to the metric that actually ran.
  finalMetric = $derived.by<
    | { kind: "speed"; bytesPerSec: number }
    | { kind: "latency"; ms: number }
    | null
  >(() => {
    const results = this.stageResults;
    const bidirectional = this.result?.bidirectional;
    if (bidirectional)
      return {
        kind: "speed",
        bytesPerSec:
          bidirectional.down.reportedBytesPerSec +
          bidirectional.up.reportedBytesPerSec,
      };
    if (results.upload)
      return { kind: "speed", bytesPerSec: results.upload.reportedBytesPerSec };
    if (results.download)
      return {
        kind: "speed",
        bytesPerSec: results.download.reportedBytesPerSec,
      };
    if (results.latency)
      return { kind: "latency", ms: results.latency.reportedMs };
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
    const recent = this.pulseLatency.slice(-20);
    if (!recent.length) return 0;
    return (recent.filter((s) => s.lost).length / recent.length) * 100;
  });

  jitterMs = $derived.by(() => {
    const recent = this.pulseLatency.slice(-30).filter((s) => !s.lost);
    if (recent.length < 2) return 0;
    let acc = 0;
    for (let i = 1; i < recent.length; i++)
      acc += Math.abs(recent[i].rttMs - recent[i - 1].rttMs);
    return acc / (recent.length - 1);
  });

  effectiveConnectivity = $derived.by<ConnectivityState>(() => {
    // A connection failure latches connectivity offline once.
    // The keepalive that follows can clear it again.
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

  transferFailures = $derived.by<StageFailure[]>(() =>
    TRANSFER_STAGES.flatMap((stage) => this.stageFailures[stage] ?? []),
  );

  activeStages = $derived.by<StageKey[]>(() =>
    MEASURED_STAGES.filter((stage) => this.config.stages[stage]),
  );

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
      this.runConnections.throughput.browserProtocol,
      this.runConnections.throughput.target?.tls,
      this.runConnections.throughput.clientIpVersion,
    ),
  );

  downloadCompensation = $derived<CompensationEstimate>(
    estimateResultCompensation(
      this.stageResults.download,
      "download",
      this.config.compensation,
      this.runConnections.throughput.browserProtocol,
      this.runConnections.throughput.target?.tls,
      this.runConnections.throughput.clientIpVersion,
    ),
  );

  uploadCompensation = $derived<CompensationEstimate>(
    estimateResultCompensation(
      this.stageResults.upload,
      "upload",
      this.config.compensation,
      this.runConnections.throughput.browserProtocol,
      this.runConnections.throughput.target?.tls,
      this.runConnections.throughput.clientIpVersion,
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
    const samples = this.throughput;
    const n = samples.length;
    if (n === 0) return 0;
    if (n === 1) return samples[0].bytesPerSec;
    const weighted = new Array<{ bytesPerSec: number; dwellMs: number }>(n);
    for (let i = 0; i < n; i++) {
      const dwellMs =
        i === 0 ? samples[1].t - samples[0].t : samples[i].t - samples[i - 1].t;
      weighted[i] = {
        bytesPerSec: samples[i].bytesPerSec,
        dwellMs: Math.max(1, dwellMs),
      };
    }
    weighted.sort((a, b) => b.bytesPerSec - a.bytesPerSec);
    let acc = 0;
    for (const entry of weighted) {
      acc += entry.dwellMs;
      if (acc >= SCALE_DWELL_MS) return entry.bytesPerSec;
    }
    return weighted[n - 1].bytesPerSec;
  });

  displayScaleBytesPerSec = $derived.by(() => {
    const cfg = this.config.visualization.throughputMaxBytesPerSec;
    if (typeof cfg === "number" && cfg > 0) return cfg;
    return sharedThroughputScale(this.#sustainedPeakBytesPerSec);
  });

  #unitIndex = $derived.by(() => {
    // The prefix follows the raw peak, not the rounded-up dial ceiling.
    // Dial headroom alone must not flip readings to "0.xx".
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

  ingest = (event: RunnerEvent) => {
    switch (event.type) {
      case "transportDiscovery":
        this.transportDiscovery = event.discovery;
        break;
      case "infra":
        this.infra = event.info;
        break;
      case "phase": {
        this.phase = event.transition.to;
        this.phaseStage = event.transition.stage;
        this.phaseStartedAtMs = event.transition.t;
        this.phaseFraction = 0;
        // Stamp the wall-clock run start once, not on every warmup segment.
        if (event.transition.from === "idle") this.startEpoch = Date.now();
        break;
      }
      case "progress":
        this.phaseFraction = event.fraction;
        this.phaseElapsedMs = event.phaseElapsedMs;
        this.phaseBudgetMs = event.phaseBudgetMs;
        this.measuring = event.measuring;
        break;
      case "stall":
        // Store only the presentation latch; measurement logic stays in the core.
        this.measuring = false;
        this.stalledSince = performance.now();
        this.stallInfo = event.info;
        break;
      case "resume":
        this.#clearStall();
        break;
      case "stageSkipped":
        this.stageFailures = {
          ...this.stageFailures,
          [event.failure.stage]: event.failure,
        };
        break;
      case "stability":
        this.liveStability[event.snapshot.phase] = event.snapshot;
        break;
      case "stageResult":
        if (event.stage === "latency") this.stageResults.latency = event.result;
        else this.stageResults[event.stage] = event.result;
        break;
      case "throughput":
        this.throughput.push(event.sample);
        if (this.throughput.length > MAX_SAMPLES) this.throughput.shift();
        break;
      case "latency":
        if (event.sample.phase === "idle") {
          this.idleLatency.push(event.sample);
          if (this.idleLatency.length > MAX_IDLE_SAMPLES)
            this.idleLatency.shift();
        } else {
          this.latency.push(event.sample);
          if (this.latency.length > MAX_SAMPLES) this.latency.shift();
        }
        break;
      case "connectivity":
        this.connectivity = event.state;
        break;
      case "complete":
        this.result = event.result;
        this.phase = "complete";
        break;
      case "error": {
        this.error = event.error;
        // A terminal error resolves any in-flight stall, so the idle/error view
        // is not stuck in "measuring=false".
        this.#clearStall();
        if (CONNECTION_FAILURE_REASONS.has(event.error.reason)) {
          this.connectivity = "offline";
        }
        const partial = event.error.partial;
        if (partial) {
          if (partial.download) this.stageResults.download = partial.download;
          if (partial.upload) this.stageResults.upload = partial.upload;
          if (partial.latency) this.stageResults.latency = partial.latency;
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
    this.liveStability = { latency: null, download: null, upload: null };
    this.stageResults = { download: null, upload: null, latency: null };
    this.stageFailures = {};
    this.result = null;
    this.error = null;
    this.activeConfig = null;
    this.activeConnections = null;
    this.startEpoch = 0;
    this.runSeq++;
  }

  latencyLanes = $derived.by<LatencyLane[]>(() =>
    MEASURED_STAGES.map((key) => {
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
    }),
  );
}

export const store = new AppStore();

const SAVE_DEBOUNCE_MS = 250;

if (typeof window !== "undefined") {
  // The inline boot script sets data-theme pre-paint.
  // This effect tracks later changes, including live OS switches under "auto".
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
