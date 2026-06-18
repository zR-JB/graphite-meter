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
  ThroughputSample,
  LatencySample,
} from "../runner/contract";
import {
  estimateLiveCompensation,
  estimateResultCompensation,
  type CompensationEstimate,
} from "../compensation";
import { quantile } from "../format";
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
  duration: { warmupMs: 1500, latencyMs: 4000, downloadMs: 10000, uploadMs: 10000 },
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
    enabled: false, // off by default → durations behave as the base build
    minCoverageRatio: 0.52,
    stabilityThreshold: 0.86,
    maxPhaseReductionRatio: 0.5,
    minLatencySamples: 8,
    minTransferSamples: 12,
  },
  visualization: { throughputMaxBps: "auto" },
};

export const DURATION_PRESETS = {
  short: { warmupMs: 1000, latencyMs: 2500, downloadMs: 5000, uploadMs: 5000 },
  medium: { warmupMs: 1500, latencyMs: 4000, downloadMs: 10000, uploadMs: 10000 },
  long: { warmupMs: 2000, latencyMs: 6000, downloadMs: 20000, uploadMs: 20000 },
} as const;

const MAX_SAMPLES = 1200; // ~ enough for a 60s run at 16Hz, ring-buffered

class ConsoleStore {
  /* ---- raw ingest (ring buffers) ---- */
  throughput = $state<ThroughputSample[]>([]);
  latency = $state<LatencySample[]>([]);

  /* ---- lifecycle ---- */
  phase = $state<Phase>("idle");
  phaseFraction = $state(0); // 0–1 within current phase
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
  errorMsg = $state<string | null>(null);
  startEpoch = $state(0);

  /* ---- config + display prefs (hydrated from localStorage, §14.1) ----
   * `loadPersisted()` deep-merges the saved blob over the defaults so a
   * missing/extra/corrupt field never crashes; first-ever load → defaults
   * (theme from system `prefers-color-scheme`). A module-scope $effect
   * (see bottom of file) writes any change back, debounced ~250ms. */
  config = $state<RunnerConfig>(structuredClone(DEFAULT_CONFIG));

  /* ---- display preferences ---- */
  unitBase = $state<"base10" | "base2">("base10"); // Mbps vs Mibit/s
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
    return { value: this.toUnit(last.bps), unit: this.unitLabel };
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
    if (this.connectivity === "offline") return "offline";
    if (this.rollingLossPct > 5) return "unstable";
    if (this.rollingLossPct > 0.5 || this.jitterMs > 30) return "degraded";
    return "connected";
  });

  elapsedMs = $derived(this.startEpoch ? Date.now() - this.startEpoch : 0);

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

  /* ============================================================
   * Overhead compensation (§13.3)
   * The store stays bps-canonical: estimates are bps in / bps out;
   * conversion to display units happens at the UI layer via toUnit.
   * ============================================================ */

  /**
   * LIVE estimate — O(1) protocol/config-only multipliers applied to the
   * current instantaneous bps. This is a $derived (not $derived.by walking
   * samples) so it recomputes only when the latest sample's bps or the
   * compensation config changes — never per-sample-iteration. Fixes
   * linerate's per-sample recompute hot-path (§13.3).
   */
  liveCompensation = $derived<CompensationEstimate>(
    estimateLiveCompensation(
      this.throughput.at(-1)?.bps ?? 0,
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
      this.result?.download ?? null,
      "download",
      this.config.compensation,
    ),
  );

  /** RESULT estimate (upload) — same memoization profile as download. */
  uploadCompensation = $derived<CompensationEstimate>(
    estimateResultCompensation(
      this.result?.upload ?? null,
      "upload",
      this.config.compensation,
    ),
  );

  get unitLabel() {
    const speed =
      this.unitKind === "bits"
        ? this.unitBase === "base10"
          ? "Mbps"
          : "Mibit/s"
        : this.unitBase === "base10"
          ? "MB/s"
          : "MiB/s";
    return speed;
  }

  toUnit(bps: number): number {
    const div = this.unitBase === "base10" ? 1e6 : 2 ** 20;
    const v = this.unitKind === "bits" ? bps / div : bps / 8 / div;
    return v;
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
        if (to === "warmup") this.startEpoch = Date.now();
        break;
      }
      case "progress":
        this.phaseFraction = e.fraction;
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
      case "error":
        this.errorMsg = e.message;
        this.phase = "error";
        break;
    }
  };

  reset() {
    this.throughput = [];
    this.latency = [];
    this.phase = "idle";
    this.phaseFraction = 0;
    this.result = null;
    this.errorMsg = null;
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
