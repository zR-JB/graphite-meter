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
import { quantile, sharedThroughputScale, rateScaleIndex, rateUnit, rateValueAt, rawRateFrom } from "../format";
import { buildSegments } from "../runner/schedule";
import {
  loadPersisted,
  savePersisted,
  systemThemeDefault,
  type ThemePref,
  type SettingsTab,
} from "./persistence";

/* ================= STAGE SELECTION (§13.4) ================= */

/** Dwell window (ms) a throughput level must be held before it can lift the
 *  gauge/chart scale to the next tier. Filters brief transient spikes out of the
 *  scale decision while still tracking a genuine plateau within ~1 sample of the
 *  dwell elapsing. ~700 ms ≈ a dozen samples at the live cadence. */
const SCALE_DWELL_MS = 700;

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
  /** Mean absolute deviation from the lane average (ms) — same measure as
   *  LatencyResult.jitterMs, per lane. Null until ≥2 valid samples. */
  jitter: number | null;
  lossRatio: number;
  count: number;
  active: boolean;
}

/** Execution order — used by the future-only live-toggle constraint. */
const STAGE_ORDER: StageKey[] = ["latency", "download", "upload"];

/* ================= DEFAULTS (§2.4) ================= */

export const DEFAULT_CONFIG: RunnerConfig = {
  // bidirectional defaults OFF — an advanced stage toggled in Settings; enabling
  // it appends a concurrent down+up phase (combined gauge + a result card).
  stages: { latency: true, download: true, upload: true, bidirectional: false },
  skipLoadedLatencyWhenStageOff: true,
  duration: { warmupMs: 800, latencyMs: 4000, downloadMs: 10000, uploadMs: 10000, bidirectionalMs: 10000 },
  pingConcurrency: "medium",
  // Advanced ceiling only — lanes are derived per-phase (RealRunner #laneBudget);
  // 6 = the full per-origin budget, so by default the auto policy is unconstrained.
  parallelStreams: 6,
  experimentalChunkedDownload: false,
  endpoint: { host: "auto", port: 443 },
  // ----- Ported config surface (§13.1); inert until Batches C/D consume it -----
  compensation: {
    enabled: true,
    // Default profile matches the common self-host case: a real LAN NIC, cleartext
    // HTTP/1.1 (the only transport wired today). applyConnectionProfile() seeds the
    // factor/param defaults below; the user picks a profile to change them.
    profile: "lan",
    transport: "http1-clear",
    factors: {
      ethernetFraming: true,
      encapsulation: false, // tunnel-only; off on a plain LAN
      tlsRecords: false, // no TLS on cleartext HTTP/1.1
      applicationFraming: false, // HTTP/1.1 has no per-DATA-frame header
      reversePathControl: true,
      lossRetransmission: true,
      receiverBias: false, // download-only browser receive-cost correction
      steadyStateRamp: false,
      browserRuntime: true,
    },
    params: {
      mtuBytes: 1500,
      ipVersion: 4,
      vlanTagged: false,
      tcpOptionsBytes: 12,
      encapsulationBytes: 60, // WireGuard IPv4 outer header (used when tunnel on)
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
    glideMs: 1100, // early-finish acceleration glide, real-time ms
  },
  visualization: { throughputMaxBytesPerSec: "auto" },
};

export const DURATION_PRESETS = {
  short: { warmupMs: 600, latencyMs: 2500, downloadMs: 5000, uploadMs: 5000, bidirectionalMs: 5000 },
  medium: { warmupMs: 800, latencyMs: 4000, downloadMs: 10000, uploadMs: 10000, bidirectionalMs: 10000 },
  long: { warmupMs: 1200, latencyMs: 6000, downloadMs: 20000, uploadMs: 20000, bidirectionalMs: 20000 },
} as const;

const MAX_SAMPLES = 1200; // ~ enough for a 60s run at 16Hz, ring-buffered
/** Idle-keepalive ring — only feeds the connectivity pulse, so it stays small. */
const MAX_IDLE_SAMPLES = 60;

/** How far past a unit boundary the peak must reach before the display steps
 *  up to the larger prefix. 1.2 → we stay in Mbit/s until ~1200 Mbit/s, then
 *  switch to Gbit/s — so the dial never reads a fresh "0.xx Gbit/s" the instant
 *  it crosses 1000; the larger scale only appears once we're clearly above it. */
const UNIT_STEP_UP_HEADROOM = 1.2;

class AppStore {
  /* ---- raw ingest (ring buffers) ----
   * `latency` holds ONLY run samples (phase latency/download/upload/…) — the
   * box plots, chart and result stats read it. Idle-keepalive pings (phase
   * "idle", including the preflight burst) land in `idleLatency` instead, so
   * they can never displace or dilute a run's samples. */
  throughput = $state<ThroughputSample[]>([]);
  latency = $state<LatencySample[]>([]);
  idleLatency = $state<LatencySample[]>([]);

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
  /* MONOTONIC `performance.now()` timestamp the current stall began (0 = live),
   * NOT epoch `Date.now()`: its only consumer is the gauge/number grind-to-zero,
   * which computes `performance.now() - stalledSince` at draw time. Mixing clocks
   * makes that delta nonsensical (huge), clamping the decay factor to 1 — i.e.
   * the value freezes at its last reading instead of easing to 0. */
  stalledSince = $state(0);
  stallInfo = $state<StallInfo | null>(null);
  /* Transport negotiation telemetry (§transport). `currentTransport` is the
   * latest attempt (which method, what status); `transportLog` is the running
   * history for a future negotiation inspector. UI surface is deferred (§9) —
   * these are store-only for now. Both cleared on reset(). */
  currentTransport = $state<TransportAttempt | null>(null);
  transportLog = $state<TransportAttempt[]>([]);
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

  connectivity = $state<ConnectivityState>("connected");
  infra = $state<InfraInfo | null>(null);
  /** Static identity + transport capabilities of the wired engine (set once by
   *  bootRunner from runner.describe(); no I/O involved). */
  engineInfo = $state<EngineInfo | null>(null);
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
  /** Stages skipped this run because they couldn't run (capability missing /
   *  connection never established). The run continues; the gauge explains a
   *  skipped transfer, the latency profile a skipped latency stage. */
  stageFailures = $state<Partial<Record<TransportRole, StageFailure>>>({});
  startEpoch = $state(0);

  /* ---- config + display prefs (hydrated from localStorage, §14.1) ----
   * `loadPersisted()` deep-merges the saved blob over the defaults so a
   * missing/extra/corrupt field never crashes; first-ever load → defaults
   * (theme defaults to "auto", i.e. system `prefers-color-scheme`). A
   * module-scope $effect (see bottom of file) writes any change back,
   * debounced ~250ms. */
  config = $state<RunnerConfig>(structuredClone(DEFAULT_CONFIG));

  /* ---- display preferences ---- */
  unitBase = $state<"base10" | "base2">("base10"); // Mbit/s vs Mibit/s
  unitKind = $state<"bits" | "bytes">("bits");

  /* ---- persisted UI prefs (single source of truth) ---- */
  /** Active theme preference — "dark" | "light" | "auto" (follows OS). Resolved
   *  to a concrete "dark"/"light" and applied to `document.documentElement[data-theme]`
   *  by an $effect below — the ONLY place the attribute is set at runtime. */
  theme = $state<ThemePref>("dark");
  /** Whether result cards surface the compensated wire-rate estimate (§14.2). */
  showWireEstimates = $state(false);
  /** User-resized docked side-panel widths (px), per side. Persisted. */
  dockWidth = $state<{ left: number; right: number }>({ left: 400, right: 400 });
  /** Last-viewed Settings tab — persisted so the panel reopens where the user
   *  left it. */
  settingsTab = $state<SettingsTab>("setup");
  /** Dev diagnostic toggle (Settings › Developer). When on, the runner/core/
   *  workers emit verbose console logs; wire.svelte.ts mirrors it into the
   *  debug logger. Persisted so it survives reloads during a debugging session. */
  debugLogging = $state(false);

  constructor() {
    const p = loadPersisted();
    this.config = p.config;
    this.unitBase = p.unitBase;
    this.unitKind = p.unitKind;
    this.theme = p.theme;
    this.showWireEstimates = p.showWireEstimates;
    this.dockWidth = p.dockWidth;
    // Only known tabs; anything else falls back to Setup.
    this.settingsTab = p.settingsTab === "developer" ? "developer" : "setup";
    this.debugLogging = p.debugLogging;
  }

  /* ================= DERIVED ================= */

  /** The single big number shown in the gauge, in the active unit. */
  liveMetric = $derived.by(() => {
    const last = this.#lastSampleForPhase();
    if (!last) return { value: 0, unit: this.unitLabel };
    return { value: this.toUnit(last.bytesPerSec), unit: this.unitLabel };
  });

  /** The headline metric to rest on at the END of a run: download if it ran,
   *  else upload, else the combined bidirectional rate, else latency.
   *  Phase-agnostic so the gauge + big number never assume download exists — a
   *  latency-only, upload-only, or bidirectional-only run all resolve to a
   *  sensible final reading instead of a stale/misread value. */
  finalMetric = $derived.by<
    { kind: "speed"; bytesPerSec: number } | { kind: "latency"; ms: number } | null
  >(() => {
    const r = this.stageResults;
    if (r.download) return { kind: "speed", bytesPerSec: r.download.reportedBytesPerSec };
    if (r.upload) return { kind: "speed", bytesPerSec: r.upload.reportedBytesPerSec };
    const bidi = this.result?.bidirectional;
    if (bidi)
      return { kind: "speed", bytesPerSec: bidi.down.reportedBytesPerSec + bidi.up.reportedBytesPerSec };
    if (r.latency) return { kind: "latency", ms: r.latency.reportedMs };
    return null;
  });

  /** Live instantaneous transfer rate the gauge + big number read. In
   *  download/upload it's the latest sample; in bidirectional it's the sum of
   *  the most recent down + up samples (the combined throughput), so the dial
   *  and number show the aggregate the phase is actually moving. */
  liveTransferBytesPerSec = $derived.by(() => {
    if (this.phase === "bidirectional") {
      let down = 0;
      let up = 0;
      for (let i = this.throughput.length - 1; i >= 0; i--) {
        const s = this.throughput[i];
        if (s.phase !== "bidirectional") break;
        if (s.dir === "down" && down === 0) down = s.bytesPerSec;
        else if (s.dir === "up" && up === 0) up = s.bytesPerSec;
        if (down > 0 && up > 0) break;
      }
      return down + up;
    }
    return this.throughput.at(-1)?.bytesPerSec ?? 0;
  });

  /** The bidirectional phase's two live lanes (latest down + up), for the
   *  result card while the phase runs. Null outside the bidirectional phase. */
  liveBidirectional = $derived.by<{ down: number; up: number } | null>(() => {
    if (this.phase !== "bidirectional") return null;
    let down = 0;
    let up = 0;
    for (let i = this.throughput.length - 1; i >= 0; i--) {
      const s = this.throughput[i];
      if (s.phase !== "bidirectional") break;
      if (s.dir === "down" && down === 0) down = s.bytesPerSec;
      else if (s.dir === "up" && up === 0) up = s.bytesPerSec;
      if (down > 0 && up > 0) break;
    }
    return { down, up };
  });

  /** The samples the connectivity pulse (dot, sparkline, live ping, loss/
   *  jitter) reads: run samples while a test is in flight, the idle keepalive
   *  otherwise (it is stopped during a run, so its buffer would be stale). */
  pulseLatency = $derived.by<LatencySample[]>(() => {
    if (this.isRunning) return this.latency;
    return this.idleLatency.length ? this.idleLatency : this.latency;
  });

  /** Most recent rtt for the connectivity pulse + live ping. */
  liveRtt = $derived(
    this.pulseLatency.length
      ? this.pulseLatency.at(-1)!.rttMs
      : (this.infra?.preTestPingMs ?? 0),
  );

  /** Rolling packet loss over last 20 latency samples (for pulse state). */
  rollingLossPct = $derived.by(() => {
    const w = this.pulseLatency.slice(-20);
    if (!w.length) return 0;
    return (w.filter((s) => s.lost).length / w.length) * 100;
  });

  jitterMs = $derived.by(() => {
    const w = this.pulseLatency.slice(-30).filter((s) => !s.lost);
    if (w.length < 2) return 0;
    let acc = 0;
    for (let i = 1; i < w.length; i++) acc += Math.abs(w[i].rttMs - w[i - 1].rttMs);
    return acc / (w.length - 1);
  });

  /** UI computes connectivity if runner doesn't push it (defensive). */
  effectiveConnectivity = $derived.by<ConnectivityState>(() => {
    // NOTE: no hard "error phase ⇒ offline" pin here — the error ingest latches
    // `connectivity = "offline"` once for connection failures, and the idle
    // keepalive (restarted by the backend after every run) is then free to
    // report recovery while the error view is still up.
    // A stall mid-run is dead air — the link is effectively offline until the
    // backend reconnects (resume clears `measuring`), so the pulse goes red.
    if (this.isRunning && !this.measuring) return "offline";
    if (this.connectivity === "offline") return "offline";
    if (this.rollingLossPct > 5) return "unstable";
    if (this.rollingLossPct > 0.5 || this.jitterMs > 30) return "degraded";
    return "connected";
  });

  /** Total run ETA at the saved config — the sum of every enabled stage's
   *  test-time budget plus its warmup, computed by the SHARED scheduler so the
   *  estimate can never drift from the real timeline (and counts bidirectional
   *  when it's on). Excludes the glide/stall, which only move the actual end. */
  totalEtaMs = $derived(buildSegments(this.config).totalMs);

  /** Test-time remaining in the active phase (budget − measured elapsed). Goes
   *  to 0 at the budget and STOPS shrinking while stalled (both inputs freeze),
   *  so a connection drop visibly pushes the run end out (§4). */
  phaseRemainingMs = $derived(Math.max(0, this.phaseBudgetMs - this.phaseElapsedMs));

  bytesTransferred = $derived(this.throughput.at(-1)?.bytesCumulative ?? 0);

  isRunning = $derived(
    !["idle", "complete", "aborted", "error"].includes(this.phase),
  );

  /** Skipped TRANSFER stages, in run order — the gauge explains these. */
  transferFailures = $derived.by<StageFailure[]>(() => {
    const out: StageFailure[] = [];
    for (const k of ["download", "upload", "bidirectional"] as const) {
      const f = this.stageFailures[k];
      if (f) out.push(f);
    }
    return out;
  });

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
      this.phase === "upload" ? "upload" : "download",
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

  /** Sustained throughput peak (bytes/s) — the highest level the link held for at
   *  least SCALE_DWELL_MS, time-weighted across BOTH transfer phases. This is the
   *  scale driver, NOT the raw peak: a brief transient spike (e.g. one 1.2 Gbit
   *  sample on a 1 Gbit line) never accumulates the dwell, so it can't push the
   *  gauge/chart to the next tier. It is monotonic non-decreasing within a run —
   *  time-at-or-above any level only grows as samples accrue — so the scale
   *  ratchets up with the plateau and never flaps down mid-run. */
  #sustainedPeakBytesPerSec = $derived.by(() => {
    const arr = this.throughput;
    const n = arr.length;
    if (n === 0) return 0;
    if (n === 1) return arr[0].bytesPerSec;
    // Weight each sample by its time gap (ms) to the previous one; the first
    // borrows the second's gap. Walk values high→low, accumulating dwell; the
    // value at which cumulative time crosses SCALE_DWELL_MS is the sustained peak.
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
    // Run still shorter than the dwell window → use the lowest level seen, so the
    // scale starts modest and grows with the ramp instead of latching a transient.
    return weighted[n - 1].v;
  });

  /** Absolute throughput scale (bytes/s) shared by the gauge AND the chart AND the
   *  display unit, so a glance at either instrument maps to the same number and
   *  the two are identically scaled. Fixed when the user pins
   *  `visualization.throughputMaxBytesPerSec`; otherwise auto — the next 1-2-5 rung
   *  above the SUSTAINED peak across BOTH transfer phases (download + upload), so
   *  up/down stay on one comparable scale and transients don't bump the tier. */
  displayScaleBytesPerSec = $derived.by(() => {
    const cfg = this.config.visualization.throughputMaxBytesPerSec;
    if (typeof cfg === "number" && cfg > 0) return cfg;
    // Headroom + the tier ladder both live in sharedThroughputScale; the idle
    // default (100 Mbit/s) is returned for a non-positive peak so ticks show.
    return sharedThroughputScale(this.#sustainedPeakBytesPerSec);
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
        this.phase = e.transition.to;
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
        this.stalledSince = performance.now();
        this.stallInfo = e.info;
        break;
      case "resume":
        // Reconnected: real samples are flowing again; presentation snaps back.
        this.measuring = true;
        this.stalledSince = 0;
        this.stallInfo = null;
        break;
      case "transport":
        // Store-only for now (no visible transport indicator yet — §9).
        this.currentTransport = e.attempt;
        this.transportLog.push(e.attempt);
        break;
      case "stageSkipped":
        this.stageFailures = { ...this.stageFailures, [e.failure.stage]: e.failure };
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
          if (this.idleLatency.length > MAX_IDLE_SAMPLES) this.idleLatency.shift();
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
        // The run is over — the stall (if one was open) is resolved into this
        // terminal error, so clear its presentation state instead of leaving a
        // stale "measuring=false" latch behind.
        this.measuring = true;
        this.stalledSince = 0;
        this.stallInfo = null;
        // Latch the pulse offline for connection-type failures (the link was
        // demonstrably dead at this moment). The restarted idle keepalive
        // pushes fresh `connectivity` events, so recovery un-latches this
        // even while the error view is still showing.
        if (
          e.error.reason === "connection-lost" ||
          e.error.reason === "timeout" ||
          e.error.reason === "preflight-failed" ||
          e.error.reason === "transport-unavailable"
        ) {
          this.connectivity = "offline";
        }
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

  /* ============================================================
   * Latency lanes (§13.5 — LatencyProfile)
   * Bucket every latency sample into one of three lanes by the
   * sample's own `phase` tag: idle (the `latency` phase), loaded-down
   * (pings during `download`), loaded-up (pings during `upload`). Each
   * lane carries the distribution stats the native profile renders:
   * min/max range, P10–P90 band, average, current, loss. Recomputes
   * only when `latency` changes (not per render). Pre-test pings carry
   * phase "idle", so they never fall into the idle (latency) lane.
   * ============================================================ */
  latencyLanes = $derived.by<LatencyLane[]>(() => {
    return (["latency", "download", "upload"] as const).map((key) => {
      // Bucket strictly by the sample's stamped phase: idle = latency-phase
      // pings; loaded = under-load pings tagged with the matching transfer phase.
      const laneSamples = this.latency.filter((s) =>
        key === "latency" ? s.phase === "latency" : s.underLoad && s.phase === key,
      );
      const valid = laneSamples.filter((s) => !s.lost);
      const sorted = valid.map((s) => s.rttMs).sort((a, b) => a - b);
      const avg = valid.length
        ? valid.reduce((sum, s) => sum + s.rttMs, 0) / valid.length
        : null;
      const jitter =
        avg != null && valid.length >= 2
          ? valid.reduce((sum, s) => sum + Math.abs(s.rttMs - avg), 0) / valid.length
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

  #lastSampleForPhase() {
    return this.throughput.at(-1) ?? null;
  }
}

export const store = new AppStore();

/* ============================================================
 * Persistence side-effects (§14.1, Batch H) — browser only.
 *
 * A single module-scope `$effect.root` owns two effects:
 *   1. THEME APPLY — resolves `store.theme` ("dark" | "light" | "auto") to a
 *      concrete "dark"/"light" and writes it to the documentElement
 *      `data-theme` attribute. This is the single runtime source of
 *      truth for the attribute (the boot script in main.ts only seeds
 *      it pre-paint to avoid a flash; this effect keeps it in sync — and,
 *      for "auto", keeps re-resolving live as the OS preference changes).
 *   2. DEBOUNCED SAVE — reads every persisted field (so any change to
 *      config / prefs re-runs it) and writes a `$state.snapshot` of the
 *      whole blob to localStorage ~250ms after the last change.
 *
 * `$state.snapshot` unwraps the reactive proxies so JSON.stringify sees
 * plain data. Guarded with `typeof window` so SSR/tests stay inert.
 * `.root` is never torn down — these live for the app's lifetime. ============================================================ */
const SAVE_DEBOUNCE_MS = 250;

if (typeof window !== "undefined") {
  // Live OS theme preference, for resolving `theme === "auto"`. Read once for
  // the initial value; the `change` listener below keeps it current so a
  // live OS toggle repaints the app immediately while in auto mode.
  let systemPrefersLight = $state(systemThemeDefault() === "light");
  if (window.matchMedia) {
    window
      .matchMedia("(prefers-color-scheme: light)")
      .addEventListener("change", (e) => {
        systemPrefersLight = e.matches;
      });
  }

  $effect.root(() => {
    // 1. Apply theme → <html data-theme>, resolving "auto" to the live OS preference.
    $effect(() => {
      const resolved =
        store.theme === "auto" ? (systemPrefersLight ? "light" : "dark") : store.theme;
      document.documentElement.setAttribute("data-theme", resolved);
    });

    // 2. Debounced write-through of all persisted fields.
    let timer: ReturnType<typeof setTimeout> | undefined;
    $effect(() => {
      // Touch every persisted field so the effect tracks them all.
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
