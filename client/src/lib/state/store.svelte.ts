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
  LatencyBucket,
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
  combineCompensationEstimates,
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
import { LatencyScaleController } from "../runner/latencyScale";
import { latencyJitterMs, upsertLatencyBucket } from "../runner/latencyBuckets";
import {
  appendThroughputSample,
  compactThroughputHistory,
  PRESENTATION_POINT_LIMIT,
} from "../runner/presentationHistory";
import { weightedMean, weightedMeanAbsoluteDeviation } from "../runner/stats";
import {
  canDisableBidirectional as canDisableBidirectionalPure,
  canToggleMeasuredStage,
  latestOneWayThroughputForPhase,
  latestBidirectionalLanes,
  sustainedRate,
  updateLiveThroughput,
} from "./stageGuards";
import {
  deriveStagePresentation,
  STAGE_ORDER,
  type StagePresentation,
} from "./stagePresentation";
import { DEFAULT_CONFIG } from "./defaults";
import {
  defaultPersisted,
  loadPersisted,
  savePersisted,
  systemThemeDefault,
  type ThemePref,
  type SettingsTab,
} from "./persistence";

const SCALE_DWELL_MS = 700;
const LIVE_RATE_SAMPLE_LIMIT = 1_024;

const MEASURED_STAGES = [
  "latency",
  "download",
  "upload",
  "bidirectional",
] as const;
export type StageKey = TransportRole;
const TRANSFER_STAGES = ["download", "upload", "bidirectional"] as const;
const TERMINAL_PHASES: readonly Phase[] = [
  "idle",
  "complete",
  "aborted",
  "error",
];
export interface LatencyLane {
  key: TransportRole;
  min: number | null;
  max: number | null;
  p10: number | null;
  p90: number | null;
  center: number | null;
  centerKind: "average" | "result";
  current: number | null;
  jitter: number | null;
  lossRatio: number;
  count: number;
  active: boolean;
}

const MAX_IDLE_SAMPLES = 60;

const UNIT_STEP_UP_HEADROOM = 1.2;

class AppStore {
  #latencyScale = new LatencyScaleController();
  startError = $state("");
  startPending = $state(false);
  throughput = $state<ThroughputSample[]>([]);
  throughputRevision = $state(0);
  liveThroughput = $state<ThroughputSample[]>([]);
  #scaleThroughput: Pick<ThroughputSample, "t" | "bytesPerSec">[] = [];
  #throughputTargetSpanMs = 0;
  #sustainedPeakBytesPerSec = $state(0);
  bytesTransferred = $state(0);
  /** Ephemeral upload visual target. Never contributes to history or results. */
  uploadPresentationBytesPerSec = $state<number | null>(null);
  /** Visual-target freshness only; it carries no rate or measurement evidence. */
  presentationRateRevision = $state({ transfer: 0, down: 0, up: 0 });
  latency = $state<LatencyBucket[]>([]);
  /** Changes only when latency history is no longer a pure tail append. */
  latencyRevision = $state(0);
  idleLatency = $state<LatencyBucket[]>([]);

  phase = $state<Phase>("idle");
  phaseStage = $state<TransportRole | null>(null);
  phaseStartedAtMs = $state(0);
  phaseFraction = $state(0);
  phaseElapsedMs = $state(0);
  phaseBudgetMs = $state(0);
  measuring = $state(true);
  stallInfo = $state<StallInfo | null>(null);
  liveStability = $state<{
    latency: StabilitySnapshot | null;
    download: StabilitySnapshot | null;
    upload: StabilitySnapshot | null;
    bidirectional: StabilitySnapshot | null;
  }>({ latency: null, download: null, upload: null, bidirectional: null });
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
  showWireEstimates = $state(true);
  dockWidth = $state<{ left: number; right: number }>({
    left: 400,
    right: 400,
  });
  settingsTab = $state<SettingsTab>("setup");
  debugLogging = $state(false);
  latencyScaleMs = $state(20);

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

  liveTransferBytesPerSec = $derived.by(() => {
    // Bidirectional is displayed as aggregate throughput: latest down + latest up.
    if (this.phase === "download" || this.phase === "upload") {
      return latestOneWayThroughputForPhase(this.phase, this.liveThroughput);
    }
    if (this.phase === "bidirectional") {
      const { down, up } = latestBidirectionalLanes(this.liveThroughput);
      return down + up;
    }
    return 0;
  });

  liveBidirectional = $derived.by<{ down: number; up: number } | null>(() => {
    if (this.phase !== "bidirectional") return null;
    return latestBidirectionalLanes(this.liveThroughput);
  });

  visualTransferBytesPerSec = $derived.by(() => {
    if (this.phase === "upload")
      return this.uploadPresentationBytesPerSec ?? this.liveTransferBytesPerSec;
    if (this.phase === "bidirectional") {
      const { down, up } = latestBidirectionalLanes(this.liveThroughput);
      return down + (this.uploadPresentationBytesPerSec ?? up);
    }
    return this.liveTransferBytesPerSec;
  });

  visualBidirectional = $derived.by<{ down: number; up: number } | null>(() => {
    const lanes = this.liveBidirectional;
    if (!lanes) return null;
    return {
      down: lanes.down,
      up: this.uploadPresentationBytesPerSec ?? lanes.up,
    };
  });

  pulseLatency = $derived.by<LatencyBucket[]>(() => {
    // While idle, the pulse reads the keepalive lane if available. During a run,
    // it reads measured samples so loss/jitter reflect the active test.
    if (this.isRunning) return this.latency;
    return this.idleLatency.length ? this.idleLatency : this.latency;
  });

  liveRtt = $derived(
    this.pulseLatency.at(-1)?.medianRttMs ?? this.infra?.preTestPingMs ?? 0,
  );

  liveLatencyLost = $derived(
    (this.pulseLatency.at(-1)?.pingCount ?? 0) > 0 &&
      this.pulseLatency.at(-1)?.medianRttMs == null,
  );

  rollingLossPct = $derived.by(() => {
    const latest = this.pulseLatency.at(-1)?.endT ?? 0;
    const recent = this.pulseLatency.filter(
      (bucket) => bucket.endT > latest - 4_000,
    );
    if (!recent.length) return 0;
    const pings = recent.reduce((sum, bucket) => sum + bucket.pingCount, 0);
    const losses = recent.reduce((sum, bucket) => sum + bucket.lossCount, 0);
    return pings ? (losses / pings) * 100 : 0;
  });

  jitterMs = $derived.by(() => {
    const latest = this.pulseLatency.at(-1)?.endT ?? 0;
    return latencyJitterMs(
      this.pulseLatency.filter((bucket) => bucket.endT > latest - 4_000),
    );
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

  isRunning = $derived(!TERMINAL_PHASES.includes(this.phase));

  transferFailures = $derived.by<StageFailure[]>(() =>
    TRANSFER_STAGES.flatMap((stage) => this.stageFailures[stage] ?? []),
  );

  /** The sole result/failure/phase status for every configured instrument. */
  stagePresentation = $derived.by<Record<TransportRole, StagePresentation>>(
    () => {
      const bidi =
        this.result?.bidirectional ?? this.error?.partial?.bidirectional;
      return Object.fromEntries(
        STAGE_ORDER.map((stage) => {
          const failure = this.stageFailures[stage] != null;
          const hasUsableResult =
            stage === "bidirectional"
              ? failure
                ? !!(bidi?.down || bidi?.up)
                : !!(bidi?.down && bidi?.up)
              : this.stageResults[stage] != null;
          return [
            stage,
            deriveStagePresentation(stage, {
              configured: this.runConfig.stages[stage],
              phase: this.phase,
              phaseStage: this.phaseStage,
              phaseFraction: this.phaseFraction,
              measuring: this.measuring,
              hasUsableResult,
              hasFailure: failure,
            }),
          ];
        }),
      ) as Record<TransportRole, StagePresentation>;
    },
  );

  activeStages = $derived.by<StageKey[]>(() =>
    MEASURED_STAGES.filter((stage) => this.config.stages[stage]),
  );

  // Mid-run toggles may only affect future stages. The current stage is already
  // wired and past stages have produced results.
  canToggleStage(stage: StageKey): boolean {
    if (stage === "bidirectional")
      return canDisableBidirectionalPure(this.phaseStage, this.isRunning);
    return canToggleMeasuredStage(stage, this.isRunning, this.phaseStage);
  }

  toggleStage(stage: StageKey): boolean {
    if (!this.canToggleStage(stage)) return false;

    const currentlyEnabled = this.config.stages[stage];
    const enabledCount = this.activeStages.length;

    if (currentlyEnabled && enabledCount <= 1) return false;

    this.config.stages[stage] = !currentlyEnabled;
    return true;
  }

  latencyEnabled = $derived(
    this.config.stages.latency || !this.config.skipLoadedLatencyWhenStageOff,
  );

  #estimateLiveWire(
    bytesPerSec: number,
    phase: "download" | "upload",
  ): CompensationEstimate {
    return estimateLiveCompensation(
      bytesPerSec,
      this.config.compensation,
      phase,
      this.runConnections.throughput.browserProtocol,
      this.runConnections.throughput.target?.tls,
      this.runConnections.throughput.clientIpVersion,
    );
  }

  #estimateResultWire(
    result: ThroughputResult | null,
    phase: "download" | "upload",
  ): CompensationEstimate {
    return estimateResultCompensation(
      result,
      phase,
      this.config.compensation,
      this.runConnections.throughput.browserProtocol,
      this.runConnections.throughput.target?.tls,
      this.runConnections.throughput.clientIpVersion,
    );
  }

  // The store remains bytes/sec-native. UI conversion happens at the edge via
  // toUnit(), which keeps compensation and scale math in one raw domain.
  liveCompensation = $derived<CompensationEstimate>(
    this.#estimateLiveWire(
      this.phase === "download" || this.phase === "upload"
        ? this.liveTransferBytesPerSec
        : 0,
      this.phase === "upload" ? "upload" : "download",
    ),
  );

  downloadCompensation = $derived<CompensationEstimate>(
    this.#estimateResultWire(this.stageResults.download, "download"),
  );

  uploadCompensation = $derived<CompensationEstimate>(
    this.#estimateResultWire(this.stageResults.upload, "upload"),
  );

  liveBidirectionalCompensation = $derived.by<CompensationEstimate>(() => {
    const lanes = this.liveBidirectional ?? { down: 0, up: 0 };
    return combineCompensationEstimates([
      this.#estimateLiveWire(lanes.down, "download"),
      this.#estimateLiveWire(lanes.up, "upload"),
    ]);
  });

  bidirectionalCompensation = $derived.by<CompensationEstimate>(() => {
    const result = this.result?.bidirectional;
    return combineCompensationEstimates([
      this.#estimateResultWire(result?.down ?? null, "download"),
      this.#estimateResultWire(result?.up ?? null, "upload"),
    ]);
  });

  #peakBytesPerSec = $state(0);

  displayScaleBytesPerSec = $derived.by(() => {
    const cfg = this.config.visualization.throughputMaxBytesPerSec;
    if (typeof cfg === "number" && cfg > 0) return cfg;
    const bidi = this.result?.bidirectional;
    const terminalPeak = Math.max(
      this.stageResults.download?.reportedBytesPerSec ?? 0,
      this.stageResults.upload?.reportedBytesPerSec ?? 0,
      (bidi?.down?.reportedBytesPerSec ?? 0) +
        (bidi?.up?.reportedBytesPerSec ?? 0),
    );
    return sharedThroughputScale(
      Math.max(this.#sustainedPeakBytesPerSec, terminalPeak),
    );
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
        const startsRun = event.transition.to === "connecting";
        if (event.transition.from === "idle") {
          this.#latencyScale.reset();
          this.latencyScaleMs = this.#latencyScale.scaleMs;
        }
        this.phase = event.transition.to;
        this.phaseStage = event.transition.stage;
        this.phaseStartedAtMs = event.transition.t;
        this.phaseFraction = 0;
        this.uploadPresentationBytesPerSec = null;
        // Stamp the wall-clock run start once, not on every warmup segment.
        if (startsRun) this.startEpoch = Date.now();
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
        this.liveThroughput = updateLiveThroughput(
          this.liveThroughput,
          event.sample,
        );
        const liveLanes =
          event.sample.phase === "bidirectional"
            ? latestBidirectionalLanes(this.liveThroughput)
            : null;
        const scaleRate = liveLanes
          ? liveLanes.down + liveLanes.up
          : event.sample.bytesPerSec;
        this.#scaleThroughput.push({
          t: event.sample.t,
          bytesPerSec: scaleRate,
        });
        while (
          this.#scaleThroughput.length > 2 &&
          this.#scaleThroughput[1].t < event.sample.t - SCALE_DWELL_MS * 2
        )
          this.#scaleThroughput.shift();
        if (this.#scaleThroughput.length > LIVE_RATE_SAMPLE_LIMIT)
          this.#scaleThroughput.shift();
        this.#sustainedPeakBytesPerSec = Math.max(
          this.#sustainedPeakBytesPerSec,
          sustainedRate(this.#scaleThroughput, SCALE_DWELL_MS),
        );
        this.bytesTransferred = event.sample.bytesCumulative;
        this.#peakBytesPerSec = Math.max(this.#peakBytesPerSec, scaleRate);
        if (
          appendThroughputSample(
            this.throughput,
            event.sample,
            PRESENTATION_POINT_LIMIT,
            this.#throughputTargetSpanMs,
          )
        )
          this.throughputRevision++;
        if (event.sample.phase === "bidirectional") {
          this.presentationRateRevision[event.sample.dir]++;
          this.presentationRateRevision.transfer++;
        } else {
          this.presentationRateRevision.transfer++;
        }
        break;
      case "uploadPresentation":
        this.uploadPresentationBytesPerSec = event.bytesPerSec;
        if (this.phase === "bidirectional") {
          this.presentationRateRevision.up++;
          this.presentationRateRevision.transfer++;
        } else if (this.phase === "upload") {
          this.presentationRateRevision.transfer++;
        }
        break;
      case "latency":
        if (event.sample.phase === "idle") {
          upsertLatencyBucket(this.idleLatency, event.sample, MAX_IDLE_SAMPLES);
        } else {
          const mutation = upsertLatencyBucket(
            this.latency,
            event.sample,
            PRESENTATION_POINT_LIMIT,
          );
          if (mutation === "structural-change") this.latencyRevision++;
          this.latencyScaleMs = this.#latencyScale.observe(event.sample);
        }
        break;
      case "connectivity":
        this.connectivity = event.state;
        break;
      case "complete":
        this.uploadPresentationBytesPerSec = null;
        this.result = event.result;
        this.phase = "complete";
        break;
      case "error": {
        this.uploadPresentationBytesPerSec = null;
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
    this.stallInfo = null;
  }

  reset() {
    this.startError = "";
    this.startPending = false;
    this.throughput = [];
    this.throughputRevision = 0;
    this.liveThroughput = [];
    this.#scaleThroughput = [];
    this.#throughputTargetSpanMs = buildSegments(this.config).totalMs;
    this.#sustainedPeakBytesPerSec = 0;
    this.bytesTransferred = 0;
    this.#peakBytesPerSec = 0;
    this.latency = [];
    this.#latencyScale.reset();
    this.latencyScaleMs = this.#latencyScale.scaleMs;
    this.phase = "idle";
    this.phaseStage = null;
    this.phaseStartedAtMs = 0;
    this.phaseFraction = 0;
    this.phaseElapsedMs = 0;
    this.phaseBudgetMs = 0;
    this.#clearStall();
    this.liveStability = {
      latency: null,
      download: null,
      upload: null,
      bidirectional: null,
    };
    this.stageResults = { download: null, upload: null, latency: null };
    this.stageFailures = {};
    this.result = null;
    this.error = null;
    this.activeConfig = null;
    this.activeConnections = null;
    this.startEpoch = 0;
    this.runSeq++;
  }

  restoreTestDisplayDefaults() {
    const defaults = defaultPersisted();
    this.config = structuredClone(defaults.config);
    this.unitBase = defaults.unitBase;
    this.unitKind = defaults.unitKind;
    this.showWireEstimates = defaults.showWireEstimates;
  }

  compactThroughputForDuration(durationMs: number) {
    if (durationMs <= this.#throughputTargetSpanMs) return;
    this.#throughputTargetSpanMs = durationMs;
    if (compactThroughputHistory(this.throughput, this.#throughputTargetSpanMs))
      this.throughputRevision++;
  }

  latencyLanes = $derived.by<LatencyLane[]>(() =>
    STAGE_ORDER.map((key) => {
      // Bucket by the sample's stamped phase. Pre-test pings are phase "idle",
      // so they never contaminate the measured latency lane.
      const laneSamples = this.latency.filter((s) =>
        key === "latency"
          ? s.phase === "latency"
          : s.underLoad && s.phase === key,
      );
      const valid = laneSamples.filter((sample) => sample.medianRttMs != null);
      const sorted = valid
        .map((sample) => sample.medianRttMs!)
        .sort((a, b) => a - b);
      const weightedRtts = valid.map((sample) => ({
        value: sample.medianRttMs!,
        weight: sample.pingCount - sample.lossCount,
      }));
      const avg = weightedMean(weightedRtts);
      const reported =
        key === "latency" ? this.stageResults.latency?.reportedMs : null;
      const centerKind = reported != null ? "result" : "average";
      const jitter =
        avg != null && valid.length >= 2
          ? weightedMeanAbsoluteDeviation(weightedRtts, avg)
          : null;
      const pingCount = laneSamples.reduce(
        (sum, sample) => sum + sample.pingCount,
        0,
      );
      const lossCount = laneSamples.reduce(
        (sum, sample) => sum + sample.lossCount,
        0,
      );
      const lossRatio = pingCount ? lossCount / pingCount : 0;
      return {
        key,
        min: sorted.at(0) ?? null,
        max:
          valid.reduce(
            (max, sample) => Math.max(max, sample.maxRttMs ?? 0),
            0,
          ) || null,
        p10: quantile(sorted, 0.1),
        p90: quantile(sorted, 0.9),
        center: reported ?? avg,
        centerKind,
        current: valid.at(-1)?.medianRttMs ?? null,
        jitter,
        lossRatio,
        count: pingCount,
        active:
          this.stagePresentation[key].status === "active" ||
          this.stagePresentation[key].status === "recovering",
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
