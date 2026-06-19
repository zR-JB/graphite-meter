/* ============================================================
 * The Graphite Meter — Reactive Store (§2.3 + §2.4)
 * Single source of truth. The UI binds to derived display
 * values, never to raw event streams directly. Ring buffers
 * cap memory and feed the canvas (read via rAF — NOT $effect).
 * ============================================================ */

import type {
  RunnerEvent,
  Phase,
  ConnectivityState,
  InfraInfo,
  RunResult,
  RunnerConfig,
  RunnerError,
  ThroughputSample,
  LatencySample,
  StabilitySnapshot,
  ThroughputResult,
  LatencyResult,
  StallInfo,
} from "../runner/contract";
import {
  estimateLiveCompensation,
  estimateResultCompensation,
  type CompensationEstimate,
} from "../compensation";
import { quantile, niceScaleUp, rateScaleIndex, rateUnit, rateValueAt, rawRateFrom } from "../format";
import {
  loadPersisted,
  savePersisted,
  type ThemePref,
} from "./persistence";

/* ================= STAGE SELECTION (§13.4) ================= */

/** The three user-selectable measured stages. */
export type StageKey = "latency" | "download" | "upload";

/** One distribution lane of the native LatencyProfile (§13.5). */
export interface LatencyLane {
  key: StageKey;
  min: number | null;
  max: number | null;
  p10: number | null;
  p90: number | null;
  average: number | null;
  current: number | null;
  lossRatio: number;
  count: number;
  active: boolean;
}

/** Execution order — used by the future-only live-toggle constraint. */
const STAGE_ORDER: StageKey[] = ["latency", "download", "upload"];

/* ================= DEFAULTS (§2.4) ================= */

export const DEFAULT_CONFIG: RunnerConfig = {
  stages: { latency: true, download: true, upload: true },
  skipLoadedLatencyWhenStageOff: true,
  duration: { warmupMs: 800, latencyMs: 4000, downloadMs: 10000, uploadMs: 10000 },
  transport: { transfer: "webtransport", latency: "websocket" },
  pingConcurrency: "medium",
  parallelStreams: 4,
  endpoint: { host: "auto", port: 443, path: "/measure" },
  // ----- Ported config surface (§13.1); inert until Batches C/D consume it -----
  compensation: {
    enabled: true,
    factors: {
      ethernetFraming: true,
      tlsRecords: true,
      applicationFraming: true,
      reversePathControl: true,
      lossRetransmission: true,
      steadyStateRamp: true,
      browserRuntime: true,
    },
    params: {
      mtuBytes: 1500,
      vlanTagged: false,
      tcpOptionsBytes: 12,
      framePayloadBytes: 16384,
      tlsRecordBytes: 5,
      aeadTagBytes: 16,
      quicConnIdBytes: 8,
      maxLossRatio: 0.12,
    },
  },
  adaptive: {
    // On by default: the "smart" stable-window result needs it; when off, every
    // phase runs full and reports its whole-phase average (§13.4).
    enabled: true,
    minCoverageRatio: 0.52,
    stabilityThreshold: 0.86,
    maxPhaseReductionRatio: 0.5,
    minLatencySamples: 8,
    minTransferSamples: 12,
    glideMs: 700, // early-finish acceleration glide, real-time ms
  },
  visualization: { throughputMaxBytesPerSec: "auto" },
};

export const DURATION_PRESETS = {
  short: { warmupMs: 600, latencyMs: 2500, downloadMs: 5000, uploadMs: 5000 },
  medium: { warmupMs: 800, latencyMs: 4000, downloadMs: 10000, uploadMs: 10000 },
  long: { warmupMs: 1200, latencyMs: 6000, downloadMs: 20000, uploadMs: 20000 },
} as const;

const MAX_SAMPLES = 1200; // ~ enough for a 60s run at 16Hz, ring-buffered

/** How far past a unit boundary the peak must reach before the display steps
 *  up to the larger prefix. 1.2 → we stay in Mbit/s until ~1200 Mbit/s, then
 *  switch to Gbit/s — so the dial never reads a fresh "0.xx Gbit/s" the instant
 *  it crosses 1000; the larger scale only appears once we're clearly above it. */
const UNIT_STEP_UP_HEADROOM = 1.2;

class ConsoleStore {
  /* ---- raw ingest (ring buffers) ---- */
  throughput = $state<ThroughputSample[]>([]);
  latency = $state<LatencySample[]>([]);

  /* ---- lifecycle ---- */
  phase = $state<Phase>("idle");
  phaseFraction = $state(0); // 0–1 within current phase (of the test-time budget)
  /* Measured test-time accrual for the active phase (§4). `phaseElapsedMs` is
   * the budget consumed; `phaseBudgetMs` is the phase's test-time budget. Both
   * freeze while stalled, so the derived `phaseRemainingMs` (budget − elapsed)
   * stops shrinking during dead air — the visible push-out of the run end. */
  phaseElapsedMs = $state(0);
  phaseBudgetMs = $state(0);
  /* Link health (§4 — presentation-only, principle 2). `measuring` is the
   * core's measured-time gate: false while a stall has frozen accrual. The
   * grind-to-zero gauge/number decay keys off `stalledSince` (wall-clock epoch
   * ms the stall began, 0 = live) + the last REAL sample — it stores/emits
   * nothing. `stallInfo` carries the reason for the transient "connection
   * lost — …" message. All cleared on resume. */
  measuring = $state(true);
  stalledSince = $state(0);
  stallInfo = $state<StallInfo | null>(null);
  /* Live measurement stability per measured phase — the single signal behind
   * the result-card pips (and, in the runner, the early-finish glide). Each
   * key fills in once its phase begins emitting; null = no read yet. */
  liveStability = $state<{
    latency: StabilitySnapshot | null;
    download: StabilitySnapshot | null;
    upload: StabilitySnapshot | null;
  }>({ latency: null, download: null, upload: null });
  /* Monotonic run counter, bumped on every reset(). Stateful canvas engines
   * (e.g. ChartEngine) watch this to drop accumulated per-run state — the
   * single source of truth for "a new run started, clear yourself". */
  runSeq = $state(0);

  /* Time windows [t0,t1] (ms since run start) per measured phase, captured
   * on phase transitions. The single source of truth for mapping a sample's
   * `t` → its phase: latency/throughput samples carry no phase field, so the
   * result-mode chart (per-phase stats) and LatencyProfile (lane bucketing)
   * both read this instead of re-deriving boundaries. `t1` stays open
   * (Infinity) for the live phase and is closed on the next transition. */
  phaseWindows = $state<Record<string, { t0: number; t1: number }>>({});
  #lastSampleT = 0;
  connectivity = $state<ConnectivityState>("connected");
  infra = $state<InfraInfo | null>(null);
  result = $state<RunResult | null>(null);
  /* Per-stage final results, each landing the instant its phase ends (before
   * the aggregate `result` on complete). The single source of truth for a
   * finished stage's headline/method/band — cards read these so a stage's real
   * result shows while later stages still run. Stages are fully independent. */
  stageResults = $state<{
    download: ThroughputResult | null;
    upload: ThroughputResult | null;
    latency: LatencyResult | null;
  }>({ download: null, upload: null, latency: null });
  /** Structured failure for the last run (null unless phase is "error"). Carries
   *  the reason, the failed phase, and any partial results (§ structured
   *  termination). User aborts are NOT errors — they are the "aborted" phase. */
  error = $state<RunnerError | null>(null);
  startEpoch = $state(0);

  /* ---- config + display prefs (hydrated from localStorage, §14.1) ----
   * `loadPersisted()` deep-merges the saved blob over the defaults so a
   * missing/extra/corrupt field never crashes; first-ever load → defaults
   * (theme from system `prefers-color-scheme`). A module-scope $effect
   * (see bottom of file) writes any change back, debounced ~250ms. */
  config = $state<RunnerConfig>(structuredClone(DEFAULT_CONFIG));

  /* ---- display preferences ---- */
  unitBase = $state<"base10" | "base2">("base10"); // Mbit/s vs Mibit/s
  unitKind = $state<"bits" | "bytes">("bits");

  /* ---- persisted UI prefs (single source of truth) ---- */
  /** Active theme. Applied to `document.documentElement[data-theme]` by an
   *  $effect below — the ONLY place the attribute is set at runtime. */
  theme = $state<ThemePref>("dark");
  /** Whether result cards surface the compensated wire-rate estimate (§14.2). */
  showWireEstimates = $state(false);
  /** User-resized docked side-panel widths (px), per side. Persisted. */
  dockWidth = $state<{ left: number; right: number }>({ left: 520, right: 420 });

  constructor() {
    const p = loadPersisted();
    this.config = p.config;
    this.unitBase = p.unitBase;
    this.unitKind = p.unitKind;
    this.theme = p.theme;
    this.showWireEstimates = p.showWireEstimates;
    this.dockWidth = p.dockWidth;
  }

  /* ================= DERIVED ================= */

  /** The single big number shown in the Reactor, in the active unit. */
  liveMetric = $derived.by(() => {
    const last = this.#lastSampleForPhase();
    if (!last) return { value: 0, unit: this.unitLabel };
    return { value: this.toUnit(last.bytesPerSec), unit: this.unitLabel };
  });

  /** The headline metric to rest on at the END of a run: download if it ran,
   *  else upload, else latency. Phase-agnostic so the gauge + big number never
   *  assume download exists — a latency-only or upload-only run resolves to a
   *  sensible final reading instead of a stale/misread value. */
  finalMetric = $derived.by<
    { kind: "speed"; bytesPerSec: number } | { kind: "latency"; ms: number } | null
  >(() => {
    const r = this.stageResults;
    if (r.download) return { kind: "speed", bytesPerSec: r.download.reportedBytesPerSec };
    if (r.upload) return { kind: "speed", bytesPerSec: r.upload.reportedBytesPerSec };
    if (r.latency) return { kind: "latency", ms: r.latency.reportedMs };
    return null;
  });

  /** Most recent rtt for the connectivity pulse + live ping. */
  liveRtt = $derived(
    this.latency.length
      ? this.latency.at(-1)!.rttMs
      : (this.infra?.preTestPingMs ?? 0),
  );

  /** Rolling packet loss over last 20 latency samples (for pulse state). */
  rollingLossPct = $derived.by(() => {
    const w = this.latency.slice(-20);
    if (!w.length) return 0;
    return (w.filter((s) => s.lost).length / w.length) * 100;
  });

  jitterMs = $derived.by(() => {
    const w = this.latency.slice(-30).filter((s) => !s.lost);
    if (w.length < 2) return 0;
    let acc = 0;
    for (let i = 1; i < w.length; i++) acc += Math.abs(w[i].rttMs - w[i - 1].rttMs);
    return acc / (w.length - 1);
  });

  /** UI computes connectivity if runner doesn't push it (defensive). */
  effectiveConnectivity = $derived.by<ConnectivityState>(() => {
    if (this.phase === "error") return "offline";
    // A stall mid-run is dead air — the link is effectively offline until the
    // backend reconnects (resume clears `measuring`), so the pulse goes red.
    if (this.isRunning && !this.measuring) return "offline";
    if (this.connectivity === "offline") return "offline";
    if (this.rollingLossPct > 5) return "unstable";
    if (this.rollingLossPct > 0.5 || this.jitterMs > 30) return "degraded";
    return "connected";
  });

  elapsedMs = $derived(this.startEpoch ? Date.now() - this.startEpoch : 0);

  /** Test-time remaining in the active phase (budget − measured elapsed). Goes
   *  to 0 at the budget and STOPS shrinking while stalled (both inputs freeze),
   *  so a connection drop visibly pushes the run end out (§4). */
  phaseRemainingMs = $derived(Math.max(0, this.phaseBudgetMs - this.phaseElapsedMs));

  bytesTransferred = $derived(this.throughput.at(-1)?.bytesCumulative ?? 0);

  isRunning = $derived(
    !["idle", "complete", "aborted", "error"].includes(this.phase),
  );

  /* ============================================================
   * Stage selection (§13.4)
   * Live stage toggling with linerate's constraints: ≥1 stage must
   * always stay enabled, and while a run is in flight only FUTURE
   * stages (after the current phase in order latency→download→upload)
   * may be toggled.
   * ============================================================ */

  /** Enabled stages in execution order — drives the timeline + rail. */
  activeStages = $derived.by<StageKey[]>(() => {
    const out: StageKey[] = [];
    if (this.config.stages.latency) out.push("latency");
    if (this.config.stages.download) out.push("download");
    if (this.config.stages.upload) out.push("upload");
    return out;
  });

  /** True when `stage` may be toggled right now. Idle → always; while
   *  running → only stages strictly after the current phase in STAGE_ORDER. */
  canToggleStage(stage: StageKey): boolean {
    if (!this.isRunning) return true;
    const currentIndex = STAGE_ORDER.indexOf(this.phase as StageKey);
    const stageIndex = STAGE_ORDER.indexOf(stage);
    return currentIndex >= 0 && stageIndex > currentIndex;
  }

  /** Toggle a stage, enforcing the ≥1-enabled floor and the future-only
   *  rule. No-ops (returns false) when the toggle is not permitted. */
  toggleStage(stage: StageKey): boolean {
    if (!this.canToggleStage(stage)) return false;

    const currentlyEnabled = this.config.stages[stage];
    const enabledCount =
      Number(this.config.stages.latency) +
      Number(this.config.stages.download) +
      Number(this.config.stages.upload);

    // Never let the last enabled stage be turned off.
    if (currentlyEnabled && enabledCount <= 1) return false;

    this.config.stages[stage] = !currentlyEnabled;
    return true;
  }

  /** Whether latency is measured & shown at all. False only when the latency
   *  stage is off AND the user opted to skip loaded latency with it — then no
   *  pings run during dl/ul and the profile/chart latency are suppressed.
   *  The single source of truth for "latency is fully disabled". */
  latencyEnabled = $derived(
    this.config.stages.latency || !this.config.skipLoadedLatencyWhenStageOff,
  );

  /* ============================================================
   * Overhead compensation (§13.3)
   * The store stays bytesPerSec-canonical: estimates are bytesPerSec in / bytesPerSec out;
   * conversion to display units happens at the UI layer via toUnit.
   * ============================================================ */

  /**
   * LIVE estimate — O(1) protocol/config-only multipliers applied to the
   * current instantaneous bytesPerSec. This is a $derived (not $derived.by walking
   * samples) so it recomputes only when the latest sample's bytesPerSec or the
   * compensation config changes — never per-sample-iteration. Fixes
   * linerate's per-sample recompute hot-path (§13.3).
   */
  liveCompensation = $derived<CompensationEstimate>(
    estimateLiveCompensation(
      this.throughput.at(-1)?.bytesPerSec ?? 0,
      this.config.compensation,
    ),
  );

  /**
   * RESULT estimate (download) — full sample-derived estimate. Recomputes
   * ONLY when `result` (or the config) changes, since both inputs are the
   * only reactive reads here; the heavy factor math runs once on `complete`.
   */
  downloadCompensation = $derived<CompensationEstimate>(
    estimateResultCompensation(
      this.stageResults.download,
      "download",
      this.config.compensation,
    ),
  );

  /** RESULT estimate (upload) — same memoization profile as download. */
  uploadCompensation = $derived<CompensationEstimate>(
    estimateResultCompensation(
      this.stageResults.upload,
      "upload",
      this.config.compensation,
    ),
  );

  /** Observed throughput peak (bytes/s) across BOTH transfer phases. Monotonic
   *  per run, so derivations off it ratchet and never jitter down mid-run. */
  #peakBytesPerSec = $derived.by(() => {
    let peak = 0;
    for (const s of this.throughput) if (s.bytesPerSec > peak) peak = s.bytesPerSec;
    return peak;
  });

  /** Absolute throughput scale (bytes/s) shared by the gauge AND the display
   *  unit, so a glance at the dial maps to the number. Fixed when the user
   *  pins `visualization.throughputMaxBytesPerSec`; otherwise auto — a large nice
   *  rung above the observed peak across BOTH transfer phases (download +
   *  upload), so up/down stay on one comparable scale. */
  displayScaleBytesPerSec = $derived.by(() => {
    const cfg = this.config.visualization.throughputMaxBytesPerSec;
    if (typeof cfg === "number" && cfg > 0) return cfg;
    if (this.#peakBytesPerSec <= 0) return 1.25e7; // idle default: 12.5 MB/s (~100 Mbit/s) so ticks show
    return niceScaleUp(this.#peakBytesPerSec * 1.08);
  });

  /** Prefix index (k/M/G…) the whole UI displays in. Derived from the observed
   *  raw-byte peak — NOT the rounded-up gauge ceiling — so a peak whose ceiling
   *  rounds up to the next unit doesn't flip the display and leave readings at
   *  "0.xx". The decision lives entirely in the raw-byte domain: a single
   *  headroom multiplier (UNIT_STEP_UP_HEADROOM) on the raw peak delays the
   *  step-up until we're comfortably past the boundary. */
  #unitIndex = $derived.by(() => {
    const cfg = this.config.visualization.throughputMaxBytesPerSec;
    // Pinned scale: track the user's fixed ceiling. Otherwise the live peak.
    const refBytesPerSec = typeof cfg === "number" && cfg > 0 ? cfg : this.#peakBytesPerSec;
    // Express the raw byte rate in the active display kind's base units (bits
    // when showing bit/s), then pick the prefix with headroom baked in.
    const baseUnits = this.unitKind === "bytes" ? refBytesPerSec : refBytesPerSec * 8;
    return rateScaleIndex(baseUnits, this.unitBase, UNIT_STEP_UP_HEADROOM);
  });

  get unitLabel() {
    return rateUnit(this.unitBase, this.unitKind, this.#unitIndex);
  }

  toUnit(bytesPerSec: number): number {
    return rateValueAt(bytesPerSec, this.unitBase, this.unitKind, this.#unitIndex);
  }

  /** Inverse of `toUnit`: a value the user typed in the active display unit →
   *  raw bytes/s for storage in the (bytes-native) config. Uses the same prefix
   *  index as `toUnit`, so editing a pinned ceiling round-trips losslessly. */
  fromUnit(displayValue: number): number {
    return rawRateFrom(displayValue, this.unitBase, this.unitKind, this.#unitIndex);
  }

  /* ================= INGEST ================= */
  ingest = (e: RunnerEvent) => {
    switch (e.type) {
      case "infra":
        this.infra = e.info;
        break;
      case "phase": {
        // Close the outgoing phase's window at the latest sample time, open
        // the incoming one. Drives `phaseWindows` (shared time→phase map).
        const to = e.transition.to;
        const prev = this.phaseWindows[this.phase];
        if (prev && prev.t1 === Infinity) prev.t1 = this.#lastSampleT;
        if (to === "download" || to === "upload" || to === "latency") {
          this.phaseWindows[to] = { t0: this.#lastSampleT, t1: Infinity };
        }
        this.phase = to;
        this.phaseFraction = 0;
        // Stamp the run clock once, when leaving idle — not on every per-stage
        // warmup (there are now several), and robust to warmupMs===0 (first
        // transition is then straight into a measurement phase).
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
        // Transient drop: freeze the "measuring" state + record when it began
        // (wall-clock) so the gauge/number can grind to 0 over ~800ms purely at
        // draw time. NO sample enters any buffer (principle 1).
        this.measuring = false;
        this.stalledSince = Date.now();
        this.stallInfo = e.info;
        break;
      case "resume":
        // Reconnected: real samples are flowing again; presentation snaps back.
        this.measuring = true;
        this.stalledSince = 0;
        this.stallInfo = null;
        break;
      case "stability":
        this.liveStability[e.snapshot.phase] = e.snapshot;
        break;
      case "stageResult":
        if (e.stage === "latency") this.stageResults.latency = e.result;
        else this.stageResults[e.stage] = e.result;
        break;
      case "throughput":
        this.#lastSampleT = e.sample.t;
        this.throughput.push(e.sample);
        if (this.throughput.length > MAX_SAMPLES) this.throughput.shift();
        break;
      case "latency":
        if (e.sample.t > this.#lastSampleT) this.#lastSampleT = e.sample.t;
        this.latency.push(e.sample);
        if (this.latency.length > MAX_SAMPLES) this.latency.shift();
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
        // Surface any partial results the failed run produced — stages that
        // finished before the failure already arrived as stageResult events,
        // but a backend may also attach them here.
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

  reset() {
    this.throughput = [];
    this.latency = [];
    this.phase = "idle";
    this.phaseFraction = 0;
    this.phaseElapsedMs = 0;
    this.phaseBudgetMs = 0;
    this.measuring = true;
    this.stalledSince = 0;
    this.stallInfo = null;
    this.liveStability = { latency: null, download: null, upload: null };
    this.stageResults = { download: null, upload: null, latency: null };
    this.result = null;
    this.error = null;
    this.startEpoch = 0;
    this.phaseWindows = {};
    this.#lastSampleT = 0;
    this.runSeq++;
  }

  /* ============================================================
   * Latency lanes (§13.5 — LatencyProfile)
   * Bucket every latency sample into one of three lanes by the
   * shared phase→time map: idle (the `latency` phase), loaded-down
   * (the `download` window), loaded-up (the `upload` window). Each
   * lane carries the distribution stats the native profile renders:
   * min/max range, P10–P90 band, average, current, loss. Recomputes
   * only when `latency` / `phaseWindows` change (not per render).
   * ============================================================ */
  latencyLanes = $derived.by<LatencyLane[]>(() => {
    const inWin = (t: number, key: StageKey) => {
      const w = this.phaseWindows[key];
      if (!w) return false;
      return t >= w.t0 && t <= (w.t1 === Infinity ? Number.POSITIVE_INFINITY : w.t1);
    };
    return (["latency", "download", "upload"] as const).map((key) => {
      // Idle = latency-phase pings; loaded = under-load pings inside the
      // respective transfer window (covers any pre/post-window stragglers).
      const laneSamples = this.latency.filter((s) =>
        key === "latency"
          ? !s.underLoad && inWin(s.t, "latency")
          : s.underLoad && inWin(s.t, key),
      );
      const valid = laneSamples.filter((s) => !s.lost);
      const sorted = valid.map((s) => s.rttMs).sort((a, b) => a - b);
      const avg = valid.length
        ? valid.reduce((sum, s) => sum + s.rttMs, 0) / valid.length
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
        lossRatio,
        count: laneSamples.length,
        active: this.phase === key,
      };
    });
  });

  #lastSampleForPhase() {
    return this.throughput.at(-1) ?? null;
  }
}

export const console = new ConsoleStore();

/* ============================================================
 * Persistence side-effects (§14.1, Batch H) — browser only.
 *
 * A single module-scope `$effect.root` owns two effects:
 *   1. THEME APPLY — writes `console.theme` to the documentElement
 *      `data-theme` attribute. This is the single runtime source of
 *      truth for the attribute (the boot script in main.ts only seeds
 *      it pre-paint to avoid a flash; this effect keeps it in sync).
 *   2. DEBOUNCED SAVE — reads every persisted field (so any change to
 *      config / prefs re-runs it) and writes a `$state.snapshot` of the
 *      whole blob to localStorage ~250ms after the last change.
 *
 * `$state.snapshot` unwraps the reactive proxies so JSON.stringify sees
 * plain data. Guarded with `typeof window` so SSR/tests stay inert.
 * `.root` is never torn down — these live for the app's lifetime. ============================================================ */
const SAVE_DEBOUNCE_MS = 250;

if (typeof window !== "undefined") {
  $effect.root(() => {
    // 1. Apply theme → <html data-theme>.
    $effect(() => {
      document.documentElement.setAttribute("data-theme", console.theme);
    });

    // 2. Debounced write-through of all persisted fields.
    let timer: ReturnType<typeof setTimeout> | undefined;
    $effect(() => {
      // Touch every persisted field so the effect tracks them all.
      const snapshot = {
        config: $state.snapshot(console.config),
        unitBase: console.unitBase,
        unitKind: console.unitKind,
        theme: console.theme,
        showWireEstimates: console.showWireEstimates,
        dockWidth: $state.snapshot(console.dockWidth),
      };
      clearTimeout(timer);
      timer = setTimeout(() => savePersisted(snapshot), SAVE_DEBOUNCE_MS);
      return () => clearTimeout(timer);
    });
  });
}
