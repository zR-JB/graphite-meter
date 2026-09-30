import { SvelteMap } from "svelte/reactivity";
import { connectionQuality } from "./connectionHealth";
import type {
  RunnerEvent,
  Phase,
  ConnectivityState,
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
  StageStatus,
} from "../runner/contract";
import {
  emptyConnectionValidation,
  latencyPathNeeded,
  needsPings,
  stageLimit,
  validatePlan,
  type ConnectionValidation,
  type ConnectionValidationState,
  type ServerView,
} from "../runner/paths";
import { presentConnections } from "../presentation/paths";
import { fmtDuration, rateUnit, rateValueAt, rawRateFrom } from "../format";
import { STAGE } from "../presentation/vocabulary";
import { latencyAxisMs, throughputScales } from "../presentation/scales";
import { Smoothed } from "../presentation/motion.svelte";
import type { LatencyProfileViewLane } from "../components/latencyProfile";
import {
  activityFor,
  adaptWarmup,
  buildSegments,
  planned,
  STAGES,
} from "../runner/schedule";
import {
  latencyLanes,
  transferredBytes,
  type MultiServerResult,
} from "../runner/measure";
import { appendThroughputSample, upsertLatencyBucket } from "../runner/series";
import {
  deriveStagePresentation,
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
  DEFAULT_HISTORY_SPLIT,
  STORAGE_KEY,
  type ThemePref,
  type ResultHistoryPreference,
  DEFAULT_HISTORY_COLUMNS,
  type HistoryColumn,
} from "./persistence";
import { BUILD } from "../buildenv";
import { buildHistoryRecord, type HistoryRecord } from "../history/types";
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
  | "keyShortcuts"
  | "resultHistoryPreference"
  | "historyColumns"
  | "dockWidth"
  | "historySplit";

class AppStore {
  serverCatalog = $state<ServerCatalog | null>(null);
  selectedServers = $state<string[]>(["self"]);
  unresolvedServers = $state.raw<SavedSelection[]>([]);
  readonly servers = new SvelteMap<string, ServerView>();
  serverMetadataLoading = $derived(
    [...this.servers.values()].some((view) => view.metadataChecking),
  );
  catalogLoading = $state(true);
  selectionValidation = $derived.by(
    (): "verified" | "checking" | "failed" | "stale" => {
      // Loading the server list is part of the check, not a stale one.
      if (this.catalogLoading) return "checking";
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
  /** Display focus only; the saved latency headline is fixed by the runner. */
  latencyFocus = $state("self");
  /** The lens over a multi-server result: "" shows all servers combined, else one server. */
  resultScope = $state("");
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
  latencyServer = $derived(
    this.serverDetails?.servers.find(
      ({ server }) => server.id === this.latencyFocus,
    ),
  );
  /** Saved summaries once complete, streamed ones while running. */
  latencySummaries = $derived.by((): LatencySummaries => {
    void this.#summaryTail;
    const saved = this.latencyServer?.latencyByStage;
    return (
      (this.phase === "complete" && saved) ||
      this.summariesByServer.get(this.latencyFocus) ||
      saved ||
      {}
    );
  });
  focusLatencyServer(id: string) {
    this.latencyFocus = id;
  }

  startError = $state("");
  preparationStatus = $state<PreparationStatus>("idle");
  /** Why Start cannot run, known before the click; a check that may still pass never blocks. */
  startBlocker = $derived.by((): string => {
    if (!this.serverCatalog)
      return this.catalogLoading
        ? ""
        : "Open Settings to retry loading the server list.";
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
    return this.stageLimitError || this.streamPlanError;
  });
  /** The longest stage the selected servers all admit (GM_MAX_STAGE_DURATION on each), and who sets it. */
  stageLimit = $derived(
    stageLimit(
      this.selectedServers.flatMap((id) => this.servers.get(id) ?? []),
    ),
  );
  /** Planned stages longer than a selected server admits, all named; the start names the server rather than dropping it. */
  stageLimitError = $derived.by((): string => {
    const { ms, server } = this.stageLimit;
    const stages = STAGES.filter(
      (key) =>
        planned(this.config, key) && this.config.duration[`${key}Ms`] > ms,
    ).map((key) => STAGE[key].label);
    return stages.length && server
      ? `${server} allows stages up to ${fmtDuration(ms, 0)}; shorten the ${new Intl.ListFormat("en-GB").format(stages)} stage${stages.length > 1 ? "s" : ""}.`
      : "";
  });
  /** Why the stream settings cannot fit the verified selection; Settings shows it by the setting. */
  streamPlanError = $derived.by((): string => {
    const servers = this.selectedServers.flatMap((id) => {
      const view = this.servers.get(id);
      return view?.paths ? [{ server: view.server, paths: view.paths }] : [];
    });
    if (servers.length !== this.selectedServers.length) return "";
    try {
      validatePlan(this.config, servers);
      return "";
    } catch (cause) {
      return cause instanceof Error ? cause.message : String(cause);
    }
  });
  preparation = $derived.by<PreparationState>(() => ({
    status:
      this.preparationStatus === "idle" &&
      this.phase === "idle" &&
      !this.catalogLoading &&
      this.startBlocker
        ? "blocked"
        : this.preparationStatus,
    throughput: STAGES.some(
      (stage) => stage !== "latency" && planned(this.config, stage),
    )
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
  /** The current transfer stage's latest sample; null between stages. */
  live = $state.raw<LiveSample | null>(null);
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
  /** The first selected server, where a run's latency starts; single-path views describe it. */
  representativeServerId = $derived(
    this.serverCatalog && this.selectedServers.length
      ? selectedInCatalogOrder(this.serverCatalog, this.selectedServers)[0].id
      : null,
  );
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
  bytesTransferred = $derived(
    this.result
      ? transferredBytes(this.result.multiServer)
      : (this.throughput.at(-1)?.bytesCumulative ?? 0),
  );
  stageResults = $state.raw<StageResults>(EMPTY_STAGE_RESULTS);
  settledStages = $state.raw<Partial<Record<TransportRole, StageStatus>>>({});
  error = $state.raw<RunnerError | null>(null);

  config = $state<RunnerConfig>(structuredClone(DEFAULT_CONFIG));
  /** The current or last run's own inputs; live settings patch its config. */
  run = $state.raw<{ config: RunnerConfig; servers: PreparedServer[] } | null>(
    null,
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
  keyShortcuts = $state(true);
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
  historySplit = $state(DEFAULT_HISTORY_SPLIT);

  constructor() {
    Object.assign(this, loadPersisted());
  }

  /** The one writer of display preferences; persistence follows by effect. */
  prefer(patch: Partial<Pick<AppStore, DisplayPreference>>) {
    Object.assign(this, patch);
  }

  // A getter, not $derived: the series grow in place, so a derived would return the same array and never notify.
  get pulseLatency(): LatencyBucket[] {
    if (this.isRunning) return this.latency;
    return this.idleLatency.length ? this.idleLatency : this.latency;
  }

  liveRtt = $derived(
    this.pulseLatency.at(-1)?.medianRttMs ??
      this.connectionValidation.latency.path?.rttMs ??
      0,
  );

  liveLatencyLost = $derived(
    (this.pulseLatency.at(-1)?.pingCount ?? 0) > 0 &&
      this.pulseLatency.at(-1)?.medianRttMs == null,
  );

  effectiveConnectivity = $derived.by<
    ConnectivityState | "checking" | "recovering"
  >(() => {
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
    } else if (!this.measuring) return "recovering";
    // Run evidence ages on the run's timeline; at idle, verified paths stand until idle latency says more.
    if (this.isRunning)
      return this.phaseStage &&
        needsPings(activityFor(this.phaseStage, this.runConfig))
        ? connectionQuality(
            this.latency,
            this.phaseStartedAtMs + this.phaseElapsedMs,
          )
        : "connected";
    if (this.connectivity === "offline" && this.idleLatency.length)
      return "offline";
    const idle = connectionQuality(this.idleLatency);
    return idle === "checking" ? "connected" : idle;
  });

  /** Phase time on the frame clock: progress events correct it, measuring keeps it moving. */
  readonly phaseClock = new Smoothed();
  /** Wall time since the run started, on the frame clock. */
  readonly runClock = new Smoothed();

  isRunning = $derived(!TERMINAL_PHASES.includes(this.phase));
  /** A new start failed, so what the stage still shows is the previous run. */
  previousRun = $derived(
    this.preparation.status === "failed" && this.phase !== "idle",
  );

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
    () =>
      Object.fromEntries(
        STAGES.map((stage) => [
          stage,
          deriveStagePresentation(stage, {
            configured: planned(this.runConfig, stage),
            settled: this.settledStages[stage],
            phase: this.phase,
            phaseStage: this.phaseStage,
            phaseFraction: this.phaseFraction,
            measuring: this.measuring,
            failure:
              this.serverDetails?.failures.find(
                (failure) =>
                  failure.stage === stage &&
                  (failure.scope === "latency") === (stage === "latency"),
              )?.reason ?? null,
          }),
        ]),
      ) as Record<TransportRole, StagePresentation>,
  );

  /** Only unstarted stages can change while a run is active. */
  canToggleStage(stage: StageKey): boolean {
    if (!this.isRunning) return true;
    if (stage === "bidirectional") return this.phaseStage !== "bidirectional";
    const current = this.phaseStage ? STAGES.indexOf(this.phaseStage) : -1;
    return current >= 0 && STAGES.indexOf(stage) > current;
  }

  latencyEnabled = $derived(latencyPathNeeded(this.config));

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

  /** Bytes the running stage has moved so far, so its card counts up live. */
  liveStageBytes = $state(0);
  #stageBase = { phase: "", bytes: 0, last: 0 };

  #ingestLive(live: LiveSample): void {
    this.live = live;
    const { t, phase, continuityId, bytes: bytesCumulative } = live;
    const base = this.#stageBase;
    if (phase !== base.phase || bytesCumulative < base.last)
      this.#stageBase = {
        phase,
        bytes: bytesCumulative < base.last ? 0 : base.last,
        last: bytesCumulative,
      };
    this.#stageBase.last = bytesCumulative;
    this.liveStageBytes = bytesCumulative - this.#stageBase.bytes;
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
      appendThroughputSample(this.#throughput, sample, this.totalEtaMs);
    }
    this.#throughputTail++;
  }

  #complete(result: RunResult): void {
    this.live = null;
    this.runClock.set(result.durationMs);
    this.result = result;
    this.settledStages = result.stages;
    this.stageResults = {
      download: result.download,
      upload: result.upload,
      latency: result.latency,
    };
    this.serverDetails = result.multiServer;
    const focus =
      this.run?.servers.find(
        ({ server }) => server.id === result.multiServer.latencyFocus,
      ) ?? this.run?.servers[0];
    // A finished run has no current stage, whichever one ran or failed last.
    this.phaseStage = null;
    this.historyCandidate = this.savingResults
      ? buildHistoryRecord(
          result,
          {
            build: BUILD.clientVersion,
            engine: focus?.paths.discovery.engineVersion ?? "unknown",
          },
          this.run?.config,
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
        upsertLatencyBucket(history, event.sample);
        if (event.serverId !== this.latencyFocus) break;
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
      case "phase": {
        const { to, stage, t } = event.transition;
        const stopped = to === "aborted" && this.phase === this.phaseStage;
        if (!stopped) {
          this.phaseStage = stage;
          this.phaseFraction = 0;
        }
        this.phase = to;
        this.phaseStartedAtMs = t;
        this.phaseElapsedMs = 0;
        // A stopped stage keeps the bytes it moved; they are measured, not a result.
        if (!stopped) this.liveStageBytes = 0;
        this.phaseClock.set(0, { snap: true });
        this.live = null;
        if (to === "connecting") {
          this.preparationStatus = "idle";
          this.runClock.set(0, { rate: 1, snap: true });
        } else if (to === "aborted") this.runClock.hold();
        break;
      }
      case "progress":
        this.runClock.sync();
        this.phaseClock.set(event.phaseElapsedMs, {
          rate: event.measuring && event.phaseBudgetMs > 0 ? 1 : 0,
          max: event.phaseBudgetMs,
        });
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
      case "stageEnd":
        this.settledStages = {
          ...this.settledStages,
          [event.stage]: event.status,
        };
        break;
      case "complete":
        this.#complete(event.result);
        break;
      case "error": {
        this.live = null;
        this.runClock.hold();
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
    this.phaseClock.set(0, { snap: true });
    this.runClock.set(0, { snap: true });
    Object.assign(this, {
      startError: "",
      preparationStatus: "idle",
      throughput: [],
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
      settledStages: {},
      result: null,
      error: null,
      run: null,
      historyCandidate: null,
      liveStageBytes: 0,
      resultScope: "",
    });
    this.#stageBase = { phase: "", bytes: 0, last: 0 };
    this.runSeq++;
  }

  restoreTestDisplayDefaults() {
    const defaults = defaultPersisted();
    this.config = structuredClone(defaults.config);
    this.unitBase = defaults.unitBase;
    this.unitKind = defaults.unitKind;
    this.showWireEstimates = defaults.showWireEstimates;
    this.keyShortcuts = defaults.keyShortcuts;
    this.resultHistoryPreference = defaults.resultHistoryPreference;
  }

  latencyLanes = $derived.by<LatencyLane[]>(() => {
    const lanes = latencyLanes(this.latencySummaries);
    return STAGES.map((key) => ({
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
      // The browser's own bar takes the chosen theme's canvas, not the system's.
      for (const meta of document.querySelectorAll<HTMLMetaElement>(
        "meta[data-scheme]",
      ))
        meta.media = meta.dataset.scheme === resolved ? "all" : "not all";
    });

    let timer: ReturnType<typeof setTimeout> | undefined;
    $effect(() => {
      const snapshot = {
        config: $state.snapshot(store.config),
        unitBase: store.unitBase,
        unitKind: store.unitKind,
        theme: store.theme,
        showWireEstimates: store.showWireEstimates,
        keyShortcuts: store.keyShortcuts,
        resultHistoryPreference: store.resultHistoryPreference,
        historyColumns: [...store.historyColumns],
        dockWidth: $state.snapshot(store.dockWidth),
        historySplit: store.historySplit,
      };
      clearTimeout(timer);
      // Not motion: settings save once edits pause.
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
