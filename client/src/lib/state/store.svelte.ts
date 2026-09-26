import { SvelteMap } from "svelte/reactivity";
import { connectionQuality } from "./connectionHealth";
import { historyWireEstimates } from "../history/wire";
import type {
  RunnerEvent,
  Phase,
  ConnectivityState,
  PreparedPaths,
  RunResult,
  RunnerConfig,
  RunnerError,
  LiveSample,
  ThroughputSample,
  LatencyBucket,
  ThroughputResult,
  LatencyResult,
  StallInfo,
  TransportRole,
  StageLatencySummary,
} from "../runner/contract";
import {
  emptyConnectionValidation,
  validateServerStreams,
  type ConnectionValidation,
  type ConnectionValidationState,
  type ServerView,
} from "../runner/paths";
import { presentConnections } from "../presentation/paths";
import {
  combineCompensationEstimates,
  type CompensationEstimate,
} from "../compensation";
import { rateUnit, rateValueAt, rawRateFrom } from "../format";
import { latencyAxisMs, throughputScales } from "../presentation/scales";
import type { LatencyProfileViewLane } from "../components/latencyProfile";
import { adaptWarmup, buildSegments } from "../runner/schedule";
import {
  latencyLanes,
  type MultiServerResult,
  type ServerFailure,
} from "../runner/measure";
import { appendThroughputSample, upsertLatencyBucket } from "../runner/series";
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
import { serverWireEstimate } from "../servers/wireEstimates";
import {
  selectedInCatalogOrder,
  type ServerCatalog,
  type SavedSelection,
} from "../servers/catalog";
import type { PreparedServer } from "../runner/run";

type PreparationStatus =
  "idle" | "blocked" | "authenticating" | "checking" | "launching" | "failed";

export interface PreparationState {
  status: PreparationStatus;
  throughput: ConnectionValidationState | "disabled";
  latency: ConnectionValidationState | "disabled";
}

type StageResults = {
  download: ThroughputResult | null;
  upload: ThroughputResult | null;
  latency: LatencyResult | null;
};
type LatencySummaries = Partial<
  Record<TransportRole, StageLatencySummary | null>
>;

const UNCHECKED: ConnectionValidation = Object.freeze(
  emptyConnectionValidation(),
);
const EMPTY_STAGE_RESULTS: StageResults = Object.freeze({
  download: null,
  upload: null,
  latency: null,
});

const NO_LATENCY: LatencyBucket[] = [];
const MAX_IDLE_SAMPLES = 60;

export type StageKey = TransportRole;
const MEASURED_STAGE_ORDER = ["latency", "download", "upload"] as const;
const TERMINAL_PHASES: readonly Phase[] = [
  "idle",
  "complete",
  "aborted",
  "error",
];

export type LatencyLane = Omit<LatencyProfileViewLane, "label" | "tone">;
/** A stage without a summary shows no statistics. */
const EMPTY_LANE = {
  min: null,
  max: null,
  p10: null,
  p90: null,
  p95: null,
  center: null,
  jitter: null,
  timeoutRatio: null,
  accountingComplete: null,
  timeoutCount: null,
  unresolvedCount: null,
  sendFailureCount: null,
  count: 0,
};

type DisplayPreference =
  | "unitBase"
  | "unitKind"
  | "theme"
  | "showWireEstimates"
  | "resultHistoryPreference"
  | "historyColumns"
  | "dockWidth";

class AppStore {
  serverCatalog = $state<ServerCatalog | null>(null);
  selectedServers = $state<string[]>(["self"]);
  unresolvedServers = $state.raw<SavedSelection[]>([]);
  readonly servers = new SvelteMap<string, ServerView>();
  serverMetadataLoading = $derived(
    [...this.servers.values()].some((view) => view.metadataChecking),
  );
  catalogLoading = $state(false);
  selectionValidation = $derived.by(
    (): "verified" | "checking" | "failed" | "stale" => {
      const states = this.selectedServers.map(
        (id) => this.servers.get(id)?.readiness ?? "unchecked",
      );
      if (this.unresolvedServers.length || !states.length) return "failed";
      if (states.includes("checking")) return "checking";
      if (states.some((state) => state === "failed" || state === "sign-in"))
        return "failed";
      return states.every((state) => state === "verified")
        ? "verified"
        : "stale";
    },
  );
  serverApproval = $state<{
    id: string;
    url: string;
    code: string;
    message?: string;
    renewUrl?: string;
  } | null>(null);
  latencySelection = $state<import("./persistence").LatencySelection>({
    mode: "primary",
    serverId: "",
  });
  primaryLatencyServer = $derived(
    this.selectedServers.includes(this.latencySelection.serverId)
      ? this.latencySelection.serverId
      : (this.selectedServers[0] ?? "self"),
  );
  /** Display focus only; the saved latency headline is fixed by the runner. */
  latencyFocus = $state("self");
  serverDetails = $state.raw<MultiServerResult | null>(null);
  /** Per-server presentation evidence; the focused server's is the latency view. */
  readonly latencyByServer = new Map<string, LatencyBucket[]>();
  readonly summariesByServer = new Map<string, LatencySummaries>();
  #latencyTail = $state(0);
  #summaryTail = $state(0);
  get latency(): LatencyBucket[] {
    void this.#latencyTail;
    return this.latencyByServer.get(this.latencyFocus) ?? NO_LATENCY;
  }
  #focused = $derived(
    this.serverDetails?.servers.find(
      ({ server }) => server.id === this.latencyFocus,
    ),
  );
  /** Saved summaries once complete, streamed ones while running. */
  latencySummaries = $derived.by((): LatencySummaries => {
    void this.#summaryTail;
    const saved = this.#focused?.latencyByStage;
    return (
      (this.phase === "complete" && saved) ||
      this.summariesByServer.get(this.latencyFocus) ||
      saved ||
      {}
    );
  });
  latencyRevision = $state(0);

  focusLatencyServer(id: string) {
    this.latencyFocus = id;
    this.latencyRevision++;
  }

  startError = $state("");
  preparationStatus = $state<PreparationStatus>("idle");
  /** Why Start cannot run, known before the click; a check that may still pass never blocks. */
  startBlocker = $derived.by((): string => {
    if (!this.serverCatalog)
      return "Open Settings to retry loading the server list.";
    if (this.unresolvedServers.length || !this.selectedServers.length)
      return "The saved selection changed. Open Settings to choose the servers to test.";
    const views = this.selectedServers.flatMap(
      (id) => this.servers.get(id) ?? [],
    );
    const blocked = views.find((view) => view.blocked);
    if (blocked)
      return views.length > 1
        ? `${blocked.server.name}: ${blocked.blocked}`
        : blocked.blocked!;
    const servers = views.flatMap(({ server, paths }) =>
      paths ? [{ id: server.id, paths }] : [],
    );
    try {
      if (servers.length === this.selectedServers.length)
        validateServerStreams(this.config, servers);
    } catch (cause) {
      return cause instanceof Error ? cause.message : String(cause);
    }
    return "";
  });
  preparation = $derived.by<PreparationState>(() => ({
    status:
      this.preparationStatus === "idle" &&
      this.phase === "idle" &&
      !this.catalogLoading &&
      this.startBlocker
        ? "blocked"
        : this.preparationStatus,
    throughput:
      this.config.stages.download ||
      this.config.stages.upload ||
      this.config.stages.bidirectional
        ? this.connectionValidation.throughput.state
        : "disabled",
    latency: this.latencyEnabled
      ? this.connectionValidation.latency.state
      : "disabled",
  }));
  preparing = $derived(
    this.preparation.status === "authenticating" ||
      this.preparation.status === "checking" ||
      this.preparation.status === "launching",
  );

  // Bulk series are mutated in place; one counter per series notifies readers.
  #throughput = $state.raw<ThroughputSample[]>([]);
  #throughputTail = $state(0);
  get throughput(): ThroughputSample[] {
    void this.#throughputTail;
    return this.#throughput;
  }
  set throughput(value: ThroughputSample[]) {
    this.#throughput = value;
    this.#throughputTail++;
  }
  /** Changes when existing points move, so incremental chart indexes rebuild. */
  throughputRevision = $state(0);
  /** The current transfer stage's latest sample; null between stages. */
  live = $state.raw<LiveSample | null>(null);
  bytesTransferred = $derived(this.throughput.at(-1)?.bytesCumulative ?? 0);
  #idleLatency = $state.raw<LatencyBucket[]>([]);
  #idleLatencyTail = $state(0);
  get idleLatency(): LatencyBucket[] {
    void this.#idleLatencyTail;
    return this.#idleLatency;
  }
  set idleLatency(value: LatencyBucket[]) {
    this.#idleLatency = value;
    this.#idleLatencyTail++;
  }

  phase = $state<Phase>("idle");
  phaseStage = $state<TransportRole | null>(null);
  phaseStartedAtMs = $state(0);
  phaseFraction = $state(0);
  phaseElapsedMs = $state(0);
  phaseBudgetMs = $state(0);
  measuring = $state(true);
  stallInfo = $state.raw<StallInfo | null>(null);
  runSeq = $state(0);

  /** The idle monitor's verdict; it stands only while its evidence does. */
  connectivity = $state<ConnectivityState>("connected");
  /** The selected server single-path views describe: the latency primary, else this server. */
  representativeServerId = $derived.by(() => {
    if (!this.serverCatalog || !this.selectedServers.length) return null;
    const selected = selectedInCatalogOrder(
      this.serverCatalog,
      this.selectedServers,
    );
    const preferred =
      this.latencySelection.mode === "primary"
        ? this.primaryLatencyServer
        : "self";
    return (selected.find((server) => server.id === preferred) ?? selected[0])
      .id;
  });
  #representative = $derived(
    this.representativeServerId
      ? this.servers.get(this.representativeServerId)
      : undefined,
  );
  transportDiscovery = $derived(this.#representative?.discovery ?? null);
  connectionValidation = $derived(
    this.#representative?.validation ?? UNCHECKED,
  );
  result = $state.raw<RunResult | null>(null);
  stageResults = $state.raw<StageResults>(EMPTY_STAGE_RESULTS);
  completedStages = $state.raw<TransportRole[]>([]);
  error = $state.raw<RunnerError | null>(null);
  /** The first server failure of each stage in that stage's own scope. */
  stageFailures = $derived.by(() => {
    const failures: Partial<Record<TransportRole, ServerFailure>> = {};
    for (const failure of this.serverDetails?.failures ?? [])
      if ((failure.scope === "latency") === (failure.stage === "latency"))
        failures[failure.stage] ??= failure;
    return failures;
  });
  startEpoch = $state(0);

  config = $state<RunnerConfig>(structuredClone(DEFAULT_CONFIG));
  /** The current or last run's own inputs; live settings patch its config. */
  run = $state.raw<{ config: RunnerConfig; servers: PreparedServer[] } | null>(
    null,
  );
  /** The headline latency server's paths, which history records describe. */
  #runPaths = $derived<PreparedPaths | null>(
    (
      this.run?.servers.find(
        (entry) => entry.server.id === this.serverDetails?.latencyFocus,
      ) ?? this.run?.servers[0]
    )?.paths ?? null,
  );
  connections = $derived(
    presentConnections(
      this.config,
      this.transportDiscovery,
      this.connectionValidation,
    ),
  );
  runConfig = $derived(this.run?.config ?? this.config);
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

  constructor() {
    Object.assign(this, loadPersisted());
  }

  /** The one writer of display preferences; persistence follows by effect. */
  prefer(patch: Partial<Pick<AppStore, DisplayPreference>>) {
    Object.assign(this, patch);
  }

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

  effectiveConnectivity = $derived.by<ConnectivityState | "checking">(() => {
    if (!this.isRunning) {
      if (
        this.preparing ||
        this.catalogLoading ||
        this.selectionValidation === "checking" ||
        this.selectionValidation === "stale"
      )
        return "checking";
      if (this.selectionValidation === "failed")
        return this.selectedServers.some(
          (id) => this.servers.get(id)?.readiness === "verified",
        )
          ? "degraded"
          : "offline";
    } else if (!this.measuring) {
      return this.phase === "connecting" || this.phase === "warmup"
        ? "checking"
        : "degraded";
    }
    if (this.isRunning) return connectionQuality(this.latency);
    // Completed-run samples stay on the chart, but cannot classify a new idle connection.
    if (this.connectivity === "offline" && this.idleLatency.length)
      return "offline";
    return connectionQuality(this.idleLatency);
  });

  phaseRemainingMs = $derived(
    Math.max(0, this.phaseBudgetMs - this.phaseElapsedMs),
  );

  isRunning = $derived(!TERMINAL_PHASES.includes(this.phase));

  /** The running plan; otherwise the next run's, adapted to the verified RTT. */
  totalEtaMs = $derived(
    buildSegments(
      (this.isRunning && this.run?.config) ||
        adaptWarmup(
          this.config,
          this.connectionValidation.latency.path?.rttMs ?? 0,
        ),
    ).totalMs,
  );

  stagePresentation = $derived.by<Record<TransportRole, StagePresentation>>(
    () => {
      const bidi = this.result?.bidirectional;
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
              finished:
                this.phase === "complete" ||
                this.completedStages.includes(stage),
              hasFailure: failure,
            }),
          ];
        }),
      ) as Record<TransportRole, StagePresentation>;
    },
  );

  /** Only unstarted stages can change while a run is active. */
  canToggleStage(stage: StageKey): boolean {
    if (!this.isRunning) return true;
    if (stage === "bidirectional") return this.phaseStage !== "bidirectional";
    const current = MEASURED_STAGE_ORDER.indexOf(
      this.phaseStage as (typeof MEASURED_STAGE_ORDER)[number],
    );
    return current >= 0 && MEASURED_STAGE_ORDER.indexOf(stage) > current;
  }

  latencyEnabled = $derived(
    this.config.stages.latency || !this.config.skipLoadedLatencyWhenStageOff,
  );

  #bidirectionalWire(
    details: MultiServerResult | null,
  ): CompensationEstimate | null {
    const estimates = [
      serverWireEstimate(details, "bidirectional", "down"),
      serverWireEstimate(details, "bidirectional", "up"),
    ];
    return estimates.every(
      (value): value is CompensationEstimate => value !== null,
    )
      ? combineCompensationEstimates(estimates)
      : null;
  }

  downloadCompensation = $derived(
    this.stageResults.download &&
      serverWireEstimate(this.serverDetails, "download", "down"),
  );

  uploadCompensation = $derived(
    this.stageResults.upload &&
      serverWireEstimate(this.serverDetails, "upload", "up"),
  );

  bidirectionalCompensation = $derived(
    this.result?.bidirectional
      ? this.#bidirectionalWire(this.serverDetails)
      : null,
  );

  scales = $derived(
    throughputScales(
      this.throughput,
      {
        ...this.stageResults,
        bidirectional: this.result?.bidirectional ?? null,
      },
      this.config.visualization.throughputMaxBytesPerSec,
      this.unitBase,
      this.unitKind,
    ),
  );
  latencyScaleMs = $derived(latencyAxisMs(this.latency, !this.isRunning));

  get unitLabel() {
    return rateUnit(this.unitBase, this.unitKind, this.scales.unitIndex);
  }

  toUnit(bytesPerSec: number): number {
    const { unitBase, unitKind, scales } = this;
    return rateValueAt(bytesPerSec, unitBase, unitKind, scales.unitIndex);
  }

  fromUnit(displayValue: number): number {
    const { unitBase, unitKind, scales } = this;
    return rawRateFrom(displayValue, unitBase, unitKind, scales.unitIndex);
  }

  #ingestLive(live: LiveSample): void {
    this.live = live;
    const { t, phase, continuityId, bytes: bytesCumulative } = live;
    for (const dir of ["down", "up"] as const) {
      const bytesPerSec = live[dir];
      if (bytesPerSec == null) continue;
      const sample = {
        t,
        bytesPerSec,
        bytesCumulative,
        dir,
        phase,
        continuityId,
      };
      if (appendThroughputSample(this.#throughput, sample, this.totalEtaMs))
        this.throughputRevision++;
    }
    this.#throughputTail++;
  }

  #complete(result: RunResult): void {
    this.live = null;
    this.result = result;
    this.stageResults = {
      download: result.download,
      upload: result.upload,
      latency: result.latency,
    };
    this.serverDetails = result.multiServer;
    this.historyCandidate = this.savingResults
      ? buildHistoryRecord(
          result,
          {
            paths: this.#runPaths,
            clientBuild: BUILD.clientVersion,
            wireEstimates: historyWireEstimates(
              this.downloadCompensation,
              this.uploadCompensation,
              result.bidirectional?.down && result.bidirectional.up
                ? this.bidirectionalCompensation
                : null,
            ),
          },
          Date.now(),
        )
      : null;
    this.phase = "complete";
  }

  ingest = (event: RunnerEvent) => {
    switch (event.type) {
      case "serverLatency": {
        if (event.sample.phase === "idle") {
          upsertLatencyBucket(
            this.#idleLatency,
            event.sample,
            MAX_IDLE_SAMPLES,
          );
          this.#idleLatencyTail++;
          break;
        }
        const history = this.latencyByServer.get(event.serverId) ?? [];
        this.latencyByServer.set(event.serverId, history);
        const moved = upsertLatencyBucket(history, event.sample);
        if (event.serverId !== this.latencyFocus) break;
        if (moved) this.latencyRevision++;
        this.#latencyTail++;
        break;
      }
      case "serverLatencySummary":
        this.summariesByServer.set(event.serverId, {
          ...this.summariesByServer.get(event.serverId),
          [event.stage]: event.summary,
        });
        this.#summaryTail++;
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
        const { to, stage, t } = event.transition;
        const from = this.phase;
        if (
          STAGE_ORDER.some((key) => key === from) &&
          to !== "aborted" &&
          to !== "error" &&
          from !== to &&
          !this.completedStages.includes(from as TransportRole)
        )
          this.completedStages = [
            ...this.completedStages,
            from as TransportRole,
          ];
        this.phase = to;
        this.phaseStage = stage;
        this.phaseStartedAtMs = t;
        this.phaseFraction = this.phaseElapsedMs = 0;
        this.live = null;
        if (to === "connecting") {
          this.preparationStatus = "idle";
          this.startEpoch = event.transition.startedAt ?? Date.now();
        }
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
        this.measuring = true;
        this.stallInfo = null;
        break;
      case "stageResult":
        this.stageResults =
          event.stage === "latency"
            ? { ...this.stageResults, latency: event.result }
            : { ...this.stageResults, [event.stage]: event.result };
        break;
      case "live":
        this.#ingestLive(event.sample);
        break;
      case "complete":
        this.#complete(event.result);
        break;
      case "error": {
        this.live = null;
        this.error = event.error;
        this.measuring = true;
        this.stallInfo = null;
        this.phase = "error";
        break;
      }
    }
  };

  reset() {
    this.latencyByServer.clear();
    this.summariesByServer.clear();
    this.#latencyTail++;
    this.#summaryTail++;
    Object.assign(this, {
      startError: "",
      preparationStatus: "idle",
      throughput: [],
      throughputRevision: 0,
      live: null,
      idleLatency: [],
      serverDetails: null,
      phase: "idle" as const,
      phaseStage: null,
      phaseStartedAtMs: 0,
      phaseFraction: 0,
      phaseElapsedMs: 0,
      phaseBudgetMs: 0,
      measuring: true,
      stallInfo: null,
      stageResults: EMPTY_STAGE_RESULTS,
      completedStages: [],
      result: null,
      error: null,
      run: null,
      startEpoch: 0,
      historyCandidate: null,
    });
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

  latencyLanes = $derived.by<LatencyLane[]>(() => {
    const lanes = latencyLanes(this.latencySummaries);
    return STAGE_ORDER.map((key) => ({
      ...EMPTY_LANE,
      ...lanes[key],
      key,
      current:
        this.latency.findLast((sample) => sample.phase === key)?.medianRttMs ??
        null,
      active: ["active", "recovering"].includes(
        this.stagePresentation[key].status,
      ),
    }));
  });
}

export const store = new AppStore();

const SAVE_DEBOUNCE_MS = 250;

export function mountStoreEffects(store: AppStore): () => void {
  const onStorage = (event: StorageEvent) => {
    if (event.key !== STORAGE_KEY) return;
    const { resultHistoryPreference, historyColumns } = loadPersisted();
    // Only a real change may reach the save effect, or two tabs rewrite each other.
    store.resultHistoryPreference = resultHistoryPreference;
    if (`${historyColumns}` !== `${store.historyColumns}`)
      store.historyColumns = historyColumns;
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
