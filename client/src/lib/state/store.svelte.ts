import type {
  RunnerEvent,
  Phase,
  ConnectivityState,
  PreparedPaths,
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
  StageFailure,
  StageLatencySummary,
  ReflectorTimingSummary,
} from "../runner/contract";
import {
  CONNECTION_FAILURE_REASONS,
  presentConnections,
  emptyConnectionValidation,
  type ConnectionValidation,
} from "../runner/connectionModel";
import {
  combineCompensationEstimates,
  estimateCompensation,
  type CompensationEstimate,
} from "../compensation";
import {
  chartThroughputScale,
  DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC,
  throughputUnitIndex,
  rateUnit,
  rateValueAt,
  rawRateFrom,
} from "../format";
import { gaugeScaleForPeak } from "../components/gaugeScale";
import { buildSegments } from "../runner/schedule";
import { LatencyScaleController } from "../runner/latencyScale";
import { latencyJitterMs, upsertLatencyBucket } from "../runner/latencyBuckets";
import {
  appendThroughputSample,
  compactThroughputHistory,
  PRESENTATION_POINT_LIMIT,
} from "../runner/presentationHistory";
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
  resolveResultHistoryPreference,
  systemThemeDefault,
  DEFAULT_DOCK_WIDTH,
  STORAGE_KEY,
  type ThemePref,
  type ResultHistoryPreference,
  DEFAULT_HISTORY_COLUMNS,
  type HistoryColumn,
} from "./persistence";
import { BUILD } from "../buildenv";
import { buildHistoryRecord, type HistoryRecord } from "../history/types";

type PreparationStatus =
  "idle" | "authenticating" | "checking" | "launching" | "failed";

export interface PreparationState {
  status: PreparationStatus;
  throughput: "checking" | "ready" | "failed" | "stale" | "disabled";
  latency: "checking" | "ready" | "failed" | "stale" | "disabled";
}

type StageResults = {
  download: ThroughputResult | null;
  upload: ThroughputResult | null;
  latency: LatencyResult | null;
};

function emptyStageResults(): StageResults {
  return { download: null, upload: null, latency: null };
}

const SCALE_DWELL_MS = 700;
const LIVE_RATE_SAMPLE_LIMIT = 1_024;

export type StageKey = TransportRole;
const TRANSFER_STAGES = ["download", "upload", "bidirectional"] as const;
const TERMINAL_PHASES: readonly Phase[] = [
  "idle",
  "complete",
  "aborted",
  "error",
];
export interface LatencyLane {
  reflectorTiming?: ReflectorTimingSummary;
  key: TransportRole;
  min: number | null;
  max: number | null;
  p10: number | null;
  p90: number | null;
  center: number | null;
  centerKind: "average" | "result";
  current: number | null;
  jitter: number | null;
  timeoutRatio: number | null;
  accountingComplete: boolean | null;
  timeoutCount: number | null;
  unresolvedCount: number | null;
  sendFailureCount: number | null;
  count: number;
  active: boolean;
}

const MAX_IDLE_SAMPLES = 60;

type LiveStability = Record<
  "latency" | "download" | "upload" | "bidirectional",
  StabilitySnapshot | null
>;

function emptyStability(): LiveStability {
  return { latency: null, download: null, upload: null, bidirectional: null };
}

import { serverWireEstimate } from "../servers/wireEstimates";

class AppStore {
  serverCatalog = $state<import("../servers/catalog").ServerCatalog | null>(
    null,
  );
  selectedServers = $state<string[]>(["self"]);
  unresolvedServers = $state<import("../servers/catalog").SavedSelection[]>([]);
  serverReadiness = $state<
    Record<
      string,
      {
        state: "unchecked" | "checking" | "ready" | "sign-in" | "failed";
        message?: string;
        checkedAt?: number;
      }
    >
  >({});
  serverDiscoveries = $state<Record<string, TransportDiscovery>>({});
  catalogLoading = $state(false);
  selectionValidation = $derived.by(
    (): "verified" | "checking" | "failed" | "stale" => {
      const states = this.selectedServers.map(
        (id) => this.serverReadiness[id]?.state ?? "unchecked",
      );
      if (this.unresolvedServers.length || !states.length) return "failed";
      if (states.includes("checking")) return "checking";
      if (states.some((state) => state === "failed" || state === "sign-in"))
        return "failed";
      return states.every((state) => state === "ready") ? "verified" : "stale";
    },
  );
  serverChooserOpen = $state(false);
  serverApproval = $state<{ id: string; url: string; message?: string } | null>(
    null,
  );
  latencySelection = $state<import("./persistence").LatencySelection>({
    mode: "primary",
    serverId: "",
  });
  primaryLatencyServer = $derived(
    this.selectedServers.includes(this.latencySelection.serverId)
      ? this.latencySelection.serverId
      : (this.selectedServers[0] ?? "self"),
  );
  latencyFocus = $state("self");
  serverDetails = $state<
    import("../servers/measurement").MultiServerResult | null
  >(null);
  latencyByServer = $state<Record<string, LatencyBucket[]>>({});
  summariesByServer = $state<
    Record<string, Partial<Record<TransportRole, StageLatencySummary | null>>>
  >({});

  focusLatencyServer(id: string) {
    this.latencyFocus = id;
    this.latency = [...(this.latencyByServer[id] ?? [])];
    this.latencySummaries = {
      ...(this.summariesByServer[id] ??
        this.serverDetails?.servers.find((server) => server.server.id === id)
          ?.latencyByStage ??
        {}),
    };
    const server = this.serverDetails?.servers.find(
      (server) => server.server.id === id,
    );
    if (server) this.stageResults.latency = server.latency;
    this.#latencyScale.reset();
    for (const sample of this.latency)
      this.latencyScaleMs = this.#latencyScale.observe(sample);
    this.latencyRevision++;
  }

  #latencyScale = new LatencyScaleController();
  startError = $state("");
  preparationStatus = $state<PreparationStatus>("idle");
  preparation = $derived.by<PreparationState>(() => ({
    status: this.preparationStatus,
    throughput:
      this.config.stages.download ||
      this.config.stages.upload ||
      this.config.stages.bidirectional
        ? this.connectionValidation.throughput.state === "verified"
          ? "ready"
          : this.connectionValidation.throughput.state
        : "disabled",
    latency: this.latencyEnabled
      ? this.connectionValidation.latency.state === "verified"
        ? "ready"
        : this.connectionValidation.latency.state
      : "disabled",
  }));
  preparing = $derived(
    this.preparation.status !== "idle" && this.preparation.status !== "failed",
  );
  throughput = $state<ThroughputSample[]>([]);
  throughputRevision = $state(0);
  liveThroughput = $state<ThroughputSample[]>([]);
  aggregateEvidence = $state(true);
  #scaleThroughput: Pick<ThroughputSample, "t" | "bytesPerSec">[] = [];
  #throughputTargetSpanMs = 0;
  #sustainedPeakBytesPerSec = $state(0);
  bytesTransferred = $state(0);
  uploadPresentationBytesPerSec = $state<number | null>(null);
  latency = $state<LatencyBucket[]>([]);
  latencySummaries = $state<
    Partial<Record<TransportRole, StageLatencySummary | null>>
  >({});
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
  liveStability = $state<LiveStability>(emptyStability());
  runSeq = $state(0);

  connectivity = $state<ConnectivityState>("connected");
  transportDiscovery = $state.raw<TransportDiscovery | null>(null);
  engineInfo = $state<EngineInfo | null>(null);
  result = $state<RunResult | null>(null);
  stageResults = $state<StageResults>(emptyStageResults());
  error = $state<RunnerError | null>(null);
  stageFailures = $state<Partial<Record<TransportRole, StageFailure>>>({});
  startEpoch = $state(0);

  config = $state<RunnerConfig>(structuredClone(DEFAULT_CONFIG));
  activeConfig = $state<RunnerConfig | null>(null);
  activePaths = $state.raw<PreparedPaths | null>(null);
  connectionValidation = $state.raw<ConnectionValidation>(
    emptyConnectionValidation(),
  );
  connections = $derived(
    presentConnections(
      this.config,
      this.transportDiscovery,
      this.connectionValidation,
    ),
  );
  runConfig = $derived(this.activeConfig ?? this.config);
  runConnections = $derived(
    this.activePaths
      ? presentConnections(
          this.runConfig,
          this.activePaths.discovery,
          this.connectionValidation,
          this.activePaths,
        )
      : this.connections,
  );
  unitBase = $state<"base10" | "base2">("base10");
  unitKind = $state<"bits" | "bytes">("bits");
  theme = $state<ThemePref>("dark");
  showWireEstimates = $state(true);
  resultHistoryPreference = $state<ResultHistoryPreference>("default");
  historyColumns = $state<HistoryColumn[]>([...DEFAULT_HISTORY_COLUMNS]);
  // Keep the completion snapshot plain because IndexedDB cannot clone proxies.
  historyCandidate = $state.raw<HistoryRecord | null>(null);
  historyWarning = $state("");
  operatorHistoryDefault = $derived.by(() => {
    if (typeof document === "undefined") return false;
    return (
      document
        .querySelector('meta[name="graphite-meter-result-history-default"]')
        ?.getAttribute("content") === "true"
    );
  });
  savingResults = $derived(
    resolveResultHistoryPreference(
      this.resultHistoryPreference,
      this.operatorHistoryDefault,
    ),
  );
  dockWidth = $state<{ left: number; right: number }>({
    ...DEFAULT_DOCK_WIDTH,
  });
  latencyScaleMs = $state(20);

  constructor() {
    Object.assign(this, loadPersisted());
  }

  liveBidirectional = $derived(
    this.phase === "bidirectional"
      ? latestBidirectionalLanes(this.liveThroughput)
      : null,
  );

  liveTransferBytesPerSec = $derived(
    this.liveBidirectional
      ? this.liveBidirectional.down + this.liveBidirectional.up
      : this.phase === "download" || this.phase === "upload"
        ? latestOneWayThroughputForPhase(this.phase, this.liveThroughput)
        : 0,
  );

  visualBidirectional = $derived(
    this.liveBidirectional && {
      down: this.liveBidirectional.down,
      up: this.uploadPresentationBytesPerSec ?? this.liveBidirectional.up,
    },
  );

  visualTransferBytesPerSec = $derived(
    this.phase === "upload"
      ? (this.uploadPresentationBytesPerSec ?? this.liveTransferBytesPerSec)
      : this.visualBidirectional
        ? this.visualBidirectional.down + this.visualBidirectional.up
        : this.liveTransferBytesPerSec,
  );

  pulseLatency = $derived.by<LatencyBucket[]>(() => {
    if (this.isRunning) return this.latency;
    return this.idleLatency.length ? this.idleLatency : this.latency;
  });

  liveRtt = $derived(
    this.pulseLatency.at(-1)?.medianRttMs ??
      this.connectionValidation.latency.path?.rttMs ??
      0,
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

  canToggleStage(stage: StageKey): boolean {
    if (stage === "bidirectional")
      return canDisableBidirectionalPure(this.phaseStage, this.isRunning);
    return canToggleMeasuredStage(stage, this.isRunning, this.phaseStage);
  }

  latencyEnabled = $derived(
    this.config.stages.latency || !this.config.skipLoadedLatencyWhenStageOff,
  );

  #estimateWire(
    bytesPerSec: number,
    stage?: "download" | "upload" | "bidirectional",
    dir?: "down" | "up",
  ): CompensationEstimate | null {
    if (this.serverDetails && stage && dir)
      return serverWireEstimate(this.serverDetails, stage, dir);
    if (
      (this.serverDetails?.selection.length ?? this.selectedServers.length) > 1
    )
      return null;
    const connection = this.runConnections.throughput;
    return estimateCompensation(
      bytesPerSec,
      connection.browserProtocol,
      connection.target?.tls,
      connection.clientIpVersion,
      connection.target?.transport,
    );
  }

  liveCompensation = $derived<CompensationEstimate | null>(
    this.#estimateWire(
      this.phase === "download" || this.phase === "upload"
        ? this.liveTransferBytesPerSec
        : 0,
      this.phase === "upload" ? "upload" : "download",
      this.phase === "upload" ? "up" : "down",
    ),
  );

  downloadCompensation = $derived<CompensationEstimate | null>(
    this.#estimateWire(
      this.stageResults.download?.reportedBytesPerSec ?? 0,
      "download",
      "down",
    ),
  );

  uploadCompensation = $derived<CompensationEstimate | null>(
    this.#estimateWire(
      this.stageResults.upload?.reportedBytesPerSec ?? 0,
      "upload",
      "up",
    ),
  );

  liveBidirectionalCompensation = $derived.by<CompensationEstimate | null>(
    () => {
      const lanes = this.liveBidirectional ?? { down: 0, up: 0 };
      const estimates = [
        this.#estimateWire(lanes.down, "bidirectional", "down"),
        this.#estimateWire(lanes.up, "bidirectional", "up"),
      ];
      return estimates.every(
        (value): value is CompensationEstimate => value !== null,
      )
        ? combineCompensationEstimates(estimates)
        : null;
    },
  );

  bidirectionalCompensation = $derived.by<CompensationEstimate | null>(() => {
    const result = this.result?.bidirectional;
    const estimates = [
      this.#estimateWire(
        result?.down?.reportedBytesPerSec ?? 0,
        "bidirectional",
        "down",
      ),
      this.#estimateWire(
        result?.up?.reportedBytesPerSec ?? 0,
        "bidirectional",
        "up",
      ),
    ];
    return estimates.every(
      (value): value is CompensationEstimate => value !== null,
    )
      ? combineCompensationEstimates(estimates)
      : null;
  });

  #peakBytesPerSec = $state(0);

  chartScaleBytesPerSec = $derived.by(() => {
    const cfg = this.config.visualization.throughputMaxBytesPerSec;
    if (typeof cfg === "number" && cfg > 0) return cfg;
    const bidi = this.result?.bidirectional;
    const terminalPeak = Math.max(
      this.stageResults.download?.reportedBytesPerSec ?? 0,
      this.stageResults.upload?.reportedBytesPerSec ?? 0,
      (bidi?.down?.reportedBytesPerSec ?? 0) +
        (bidi?.up?.reportedBytesPerSec ?? 0),
    );
    return chartThroughputScale(
      Math.max(this.#sustainedPeakBytesPerSec, terminalPeak),
    );
  });

  gaugeScaleBytesPerSec = $derived.by(() => {
    const cfg = this.config.visualization.throughputMaxBytesPerSec;
    const bidi = this.result?.bidirectional;
    const terminalPeak = Math.max(
      this.stageResults.download?.reportedBytesPerSec ?? 0,
      this.stageResults.upload?.reportedBytesPerSec ?? 0,
      (bidi?.down?.reportedBytesPerSec ?? 0) +
        (bidi?.up?.reportedBytesPerSec ?? 0),
    );
    if (typeof cfg === "number" && cfg > 0) return gaugeScaleForPeak(cfg);
    const scalePeak = Math.max(
      this.#sustainedPeakBytesPerSec,
      terminalPeak,
      this.#unitIndex < 2 ? this.#peakBytesPerSec : 0,
    );
    return gaugeScaleForPeak(scalePeak, {
      minimumBitsPerSec: this.#unitIndex >= 2 ? 1_000_000_000 : undefined,
    });
  });

  #unitIndex = $derived.by(() => {
    const cfg = this.config.visualization.throughputMaxBytesPerSec;
    const refBytesPerSec =
      typeof cfg === "number" && cfg > 0
        ? cfg
        : this.#peakBytesPerSec > 0
          ? this.#peakBytesPerSec
          : DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC;
    return throughputUnitIndex(refBytesPerSec, this.unitBase, this.unitKind);
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
      case "aggregateEvidence":
        this.aggregateEvidence = event.available;
        if (!event.available) {
          this.liveThroughput = [];
          this.uploadPresentationBytesPerSec = null;
        }
        break;
      case "serverLatency": {
        const history = (this.latencyByServer[event.serverId] ??= []);
        upsertLatencyBucket(history, event.sample, PRESENTATION_POINT_LIMIT);
        if (event.serverId === this.latencyFocus)
          this.ingest({ type: "latency", sample: event.sample });
        break;
      }
      case "serverLatencySummary":
        (this.summariesByServer[event.serverId] ??= {})[event.stage] =
          event.summary;
        if (event.serverId === this.latencyFocus)
          this.latencySummaries[event.stage] = event.summary;
        break;
      case "serverDetails":
        this.serverDetails = event.details;
        break;
      case "serverFailure":
        if (this.serverDetails)
          this.serverDetails = {
            ...this.serverDetails,
            participants: event.participants,
            failures: [...this.serverDetails.failures, event.failure],
          };
        break;

      case "phase": {
        if (event.transition.from === "idle") {
          this.#latencyScale.reset();
          this.latencyScaleMs = this.#latencyScale.scaleMs;
        }
        this.phase = event.transition.to;
        this.phaseStage = event.transition.stage;
        this.phaseStartedAtMs = event.transition.t;
        this.phaseFraction = 0;
        this.uploadPresentationBytesPerSec = null;
        if (event.transition.to === "connecting") {
          this.preparationStatus = "idle";
        }
        if (event.transition.to === "connecting") this.startEpoch = Date.now();
        break;
      }
      case "progress":
        this.phaseFraction = event.fraction;
        this.phaseElapsedMs = event.phaseElapsedMs;
        this.phaseBudgetMs = event.phaseBudgetMs;
        this.measuring = event.measuring;
        break;
      case "stall":
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
        break;
      case "uploadPresentation":
        this.uploadPresentationBytesPerSec = event.bytesPerSec;
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
      case "latencySummary":
        this.latencySummaries[event.stage] = event.summary;
        break;
      case "connectivity":
        this.connectivity = event.state;
        break;
      case "complete":
        this.uploadPresentationBytesPerSec = null;
        this.result = event.result;
        this.serverDetails = event.result.multiServer ?? null;
        this.latencySummaries = event.result.latencyByStage;
        if (this.savingResults) {
          const estimate = (
            value: ThroughputResult | null,
            stage: "download" | "upload" | "bidirectional",
            dir: "down" | "up",
          ) =>
            value
              ? this.#estimateWire(value.reportedBytesPerSec, stage, dir)
              : null;
          const downloadWire = estimate(
            event.result.download,
            "download",
            "down",
          );
          const uploadWire = estimate(event.result.upload, "upload", "up");
          const bidiEstimates = event.result.bidirectional
            ? [
                estimate(
                  event.result.bidirectional.down,
                  "bidirectional",
                  "down",
                ),
                estimate(event.result.bidirectional.up, "bidirectional", "up"),
              ].filter((value): value is CompensationEstimate => value !== null)
            : [];
          const bidiWire =
            bidiEstimates.length === 2
              ? combineCompensationEstimates(bidiEstimates)
              : null;
          this.historyCandidate = buildHistoryRecord(
            event.result,
            {
              paths: this.activePaths,
              clientBuild: BUILD.clientVersion,
              wireDownloadBytesPerSec:
                downloadWire?.estimatedBytesPerSec ?? null,
              wireUploadBytesPerSec: uploadWire?.estimatedBytesPerSec ?? null,
              wireBidirectionalBytesPerSec:
                bidiWire?.estimatedBytesPerSec ?? null,
            },
            Date.now(),
          );
        } else this.historyCandidate = null;
        this.phase = "complete";
        break;
      case "error": {
        this.uploadPresentationBytesPerSec = null;
        this.error = event.error;
        this.#clearStall();
        if (CONNECTION_FAILURE_REASONS.has(event.error.reason)) {
          this.connectivity = "offline";
        }
        const partial = event.error.partial;
        if (partial?.download) this.stageResults.download = partial.download;
        if (partial?.upload) this.stageResults.upload = partial.upload;
        if (partial?.latency) this.stageResults.latency = partial.latency;
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
    Object.assign(this, {
      startError: "",
      preparationStatus: "idle",
      throughput: [],
      throughputRevision: 0,
      liveThroughput: [],
      aggregateEvidence: true,
      bytesTransferred: 0,
      latency: [],
      latencyByServer: {},
      summariesByServer: {},
      serverDetails: null,
      latencySummaries: {},
      phase: "idle" as const,
      phaseStage: null,
      phaseStartedAtMs: 0,
      phaseFraction: 0,
      phaseElapsedMs: 0,
      phaseBudgetMs: 0,
      measuring: true,
      stallInfo: null,
      liveStability: emptyStability(),
      stageResults: emptyStageResults(),
      stageFailures: {},
      result: null,
      error: null,
      activeConfig: null,
      activePaths: null,
      startEpoch: 0,
      historyCandidate: null,
    });
    this.#scaleThroughput = [];
    this.#throughputTargetSpanMs = buildSegments(this.config).totalMs;
    this.#sustainedPeakBytesPerSec = 0;
    this.#peakBytesPerSec = 0;
    this.#latencyScale.reset();
    this.latencyScaleMs = this.#latencyScale.scaleMs;
    this.runSeq++;
  }

  restoreTestDisplayDefaults() {
    const defaults = defaultPersisted();
    this.latencySelection = { ...defaults.latencySelection };
    this.config = structuredClone(defaults.config);
    this.unitBase = defaults.unitBase;
    this.unitKind = defaults.unitKind;
    this.showWireEstimates = defaults.showWireEstimates;
    this.resultHistoryPreference = defaults.resultHistoryPreference;
  }

  compactThroughputForDuration(durationMs: number) {
    if (durationMs <= this.#throughputTargetSpanMs) return;
    this.#throughputTargetSpanMs = durationMs;
    if (compactThroughputHistory(this.throughput, this.#throughputTargetSpanMs))
      this.throughputRevision++;
  }

  latencyLanes = $derived.by<LatencyLane[]>(() =>
    STAGE_ORDER.map((key) => {
      const summary = this.latencySummaries[key];
      const reported =
        key === "latency" ? this.stageResults.latency?.reportedMs : null;
      const latest = this.latency.findLast(
        (sample) => sample.phase === key,
      )?.medianRttMs;
      return {
        key,
        min: summary?.minMs ?? null,
        max: summary?.maxMs ?? null,
        p10: summary?.p10Ms ?? null,
        p90: summary?.p90Ms ?? null,
        center: reported ?? summary?.meanMs ?? null,
        centerKind: reported != null ? "result" : "average",
        current: latest ?? null,
        jitter: summary?.jitterMs ?? null,
        reflectorTiming: summary?.reflectorTiming,
        timeoutRatio: summary?.probeCount
          ? summary.timeoutCount / summary.probeCount
          : null,
        accountingComplete: summary?.accountingComplete ?? null,
        timeoutCount: summary?.timeoutCount ?? null,
        unresolvedCount: summary?.unresolvedCount ?? null,
        sendFailureCount: summary?.sendFailureCount ?? null,
        count: summary?.probeCount ?? 0,
        active: ["active", "recovering"].includes(
          this.stagePresentation[key].status,
        ),
      };
    }),
  );
}

export const store = new AppStore();

const SAVE_DEBOUNCE_MS = 250;

export function mountStoreEffects(store: AppStore): () => void {
  const onStorage = (event: StorageEvent) => {
    if (event.key !== STORAGE_KEY) return;
    const persisted = loadPersisted();
    store.resultHistoryPreference = persisted.resultHistoryPreference;
    store.historyColumns = persisted.historyColumns;
  };
  window.addEventListener("storage", onStorage);
  let systemPrefersLight = $state(systemThemeDefault() === "light");
  const media = window.matchMedia?.("(prefers-color-scheme: light)");
  const onThemeChange = (event: MediaQueryListEvent) => {
    systemPrefersLight = event.matches;
  };
  media?.addEventListener("change", onThemeChange);

  const disposeEffects = $effect.root(() => {
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
      const snapshot = {
        latencySelection: $state.snapshot(store.latencySelection),
        config: $state.snapshot(store.config),
        unitBase: store.unitBase,
        unitKind: store.unitKind,
        theme: store.theme,
        showWireEstimates: store.showWireEstimates,
        resultHistoryPreference: store.resultHistoryPreference,
        historyColumns: [...store.historyColumns],
        dockWidth: $state.snapshot(store.dockWidth),
      };
      clearTimeout(timer);
      timer = setTimeout(() => savePersisted(snapshot), SAVE_DEBOUNCE_MS);
      return () => clearTimeout(timer);
    });
  });
  return () => {
    disposeEffects();
    window.removeEventListener("storage", onStorage);
    media?.removeEventListener("change", onThemeChange);
  };
}
