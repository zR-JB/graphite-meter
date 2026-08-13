// One transfer direction: its lanes, their restarts and the byte accounting
// that turns them into samples. A bidirectional stage runs two of them.
import type { CoreHost } from "../core";
import type { FlowDirection, PhaseActivity, RecoveryCause } from "../contract";
import { debugEnabled, dlog, fmtRate, DebugWindow } from "../../debug";
import { redirectToLogin } from "../../auth";
import { laneStaggerMs } from "./backendPure";
import type { ByteLane, LaneEvents, WtProgressRelay } from "./byteLane";
import {
  DIRECTION_PROGRESS_WINDOW_MS,
  LANE_RESTART_BACKOFF_MS,
} from "./budgets";

// Match the core/dummy cadence so both engines feed the UI at the same rate.
const THROUGHPUT_CADENCE_MS = 60;

// Stagger lanes so their TCP slow-start/loss cycles do not line up perfectly.
const LANE_STAGGER_MS = 75;

/** Per-lane byte windows awaiting the next aggregate sample. Each lane reports
 *  its own receive elapsed time, so the rate sums matching per-lane terms. */
interface ClientByteAggregation {
  pendingLaneBytes: number[];
  pendingLaneElapsedSec: number[];
  timer: ReturnType<typeof setInterval> | null;
  debug: DebugWindow;
  lastAggregateAt: number;
}

/** What a direction needs from the stage that owns it. */
export interface DirectionHost {
  host: () => CoreHost;
  /** Whether a sample can prove the whole stage live. A healthy sibling's
   *  bytes remain accounted while another required direction is stalled. */
  sampleProvesStageLiveness?: () => boolean;
  /** This direction's stall state flipped; the stage combines the directions. */
  stallChanged: (
    detail?: string,
    cause?: RecoveryCause,
    direction?: FlowDirection,
  ) => void;
  /** A server upload-progress record relayed by a session lane. */
  uploadProgress: (msg: WtProgressRelay, generation: number) => void;
  /** Local upload completion metadata for an isolated visual bridge. */
  uploadPresentationHint?: (
    lane: number,
    bytes: number,
    elapsedMs: number,
    generation: number,
  ) => void;
  /** Open the upload meter's measured window. */
  beginUploadMeasure: () => void;
  /** Release every direction of the stage. */
  discardTransfer: () => void;
}

export interface DirectionOptions {
  dir: FlowDirection;
  /** The stage that owns this direction, and the target for failStage: a
   *  bidirectional lane failure reports against "bidirectional". */
  stage: PhaseActivity["stage"];
  /** Lanes for this direction, resolved from the stream policy at prime time. */
  laneCount: number;
  /** The stage's warmup window, which bounds the lane spawn stagger. */
  warmupMs: number;
  /** Builds lane `i`, so a restart reopens it without the prime context. */
  lane: (i: number, events: LaneEvents) => ByteLane;
  host: DirectionHost;
}

/** Whether the combined transfer stage is stalled by its required directions. */
export function transferStageStalled(
  directions: Iterable<Pick<TransferDirection, "stalled">>,
): boolean {
  return Array.from(directions).some((direction) => direction.stalled);
}

export class TransferDirection {
  readonly dir: FlowDirection;
  readonly stage: PhaseActivity["stage"];
  readonly laneCount: number;
  /** The URL each lane (re)starts against, by index. */
  streamUrls: string[] = [];
  /** Replaced when this direction rides a session instead of fetch lanes. */
  newLane: (i: number, events: LaneEvents) => ByteLane;
  /** True between measure() and the stage end. Gates pushing samples. */
  measuring = false;
  /** True while THIS direction is stalled. */
  stalled = false;

  #deps: DirectionHost;
  /** Per-lane spawn delay; 0 means lanes spawn together. */
  #staggerMs: number;
  /** One lane per parallel stream, indexed by stream number. */
  #lanes: (ByteLane | null)[] = [];
  /** Per-lane stagger or restart timer: a lane is never pending both at once. */
  #timers: (ReturnType<typeof setTimeout> | null)[] = [];
  /** Client byte accounting, download only: the /upload/progress channel is
   *  upload's sole byte source. */
  #aggregation: ClientByteAggregation | null;
  /** Monotonic measurement epoch. Warmup reports carry seq=0, so late messages
   *  from an old epoch are dropped at the warmup/measure boundary. */
  #measureSeq = 0;
  /** False once the stage released this direction, so a released lane never
   *  spawns workers no one owns. */
  #live = true;
  /** True while graceful lane shutdown can still deliver its final bytes. */
  #stopping = false;
  /** Per-direction measured-byte watchdog. Upload writer completions do not
   *  touch it; only receiver-authoritative progress does. */
  #progressTimer: ReturnType<typeof setTimeout> | null = null;
  /** Captured into each upload-lane callback so an old worker cannot cross a
   * session rotation boundary after a replacement has started. */
  #uploadGeneration = 0;

  constructor(opts: DirectionOptions) {
    this.dir = opts.dir;
    this.stage = opts.stage;
    this.laneCount = opts.laneCount;
    this.newLane = opts.lane;
    this.#deps = opts.host;
    // Bound the stagger so the last lane still spawns within half the warmup;
    // 0 with no warmup, so lanes spawn together, clear of the measured window.
    this.#staggerMs = laneStaggerMs(
      opts.laneCount,
      opts.warmupMs,
      LANE_STAGGER_MS,
    );
    this.#aggregation =
      opts.dir === "down"
        ? {
            pendingLaneBytes: [],
            pendingLaneElapsedSec: [],
            timer: null,
            debug: new DebugWindow(),
            lastAggregateAt: 0,
          }
        : null;
  }

  /** Open a lane per URL, staggered per index so lanes do not slow-start in
   *  lockstep. Later lanes fire from #timers, within the warmup window. */
  spawn(urls: string[]): void {
    this.streamUrls = urls;
    for (let i = 0; i < urls.length; i++) {
      const delay = i * this.#staggerMs;
      if (delay <= 0) {
        this.#startLane(i);
        continue;
      }
      this.#timers[i] = setTimeout(() => {
        this.#timers[i] = null;
        this.#startLane(i);
      }, delay);
    }
  }

  setUploadGeneration(generation: number): void {
    this.#uploadGeneration = generation;
  }

  /** Begin measuring the lanes opened at prime time and NEVER reopened:
   *  re-spawning throws away the warmed congestion window. */
  measure(): void {
    this.measuring = true;
    this.#armProgressWatchdog();
    const aggregation = this.#aggregation;
    if (aggregation) {
      aggregation.pendingLaneBytes = Array(this.laneCount).fill(0);
      aggregation.pendingLaneElapsedSec = Array(this.laneCount).fill(0);
    }
    if (this.dir === "down") {
      this.#measureSeq++;
      for (const lane of this.#lanes) lane?.measure(this.#measureSeq);
    } else {
      this.#deps.beginUploadMeasure();
    }
    if (aggregation) {
      aggregation.lastAggregateAt = performance.now();
      aggregation.debug.reset(aggregation.lastAggregateAt);
      aggregation.timer = setInterval(
        () => this.#aggregate(aggregation),
        THROUGHPUT_CADENCE_MS,
      );
    }
  }

  /** Latch this direction's own stall state and report the edge to the stage. */
  setStalled(stalled: boolean, detail?: string, cause?: RecoveryCause): void {
    if (this.stalled === stalled) return;
    this.stalled = stalled;
    this.#deps.stallChanged(detail, cause, this.dir);
  }

  /** One positive measured byte delta for this direction. Re-arms its own
   *  silence bound and recovers only this direction's stalled edge. */
  noteMeasuredProgress(bytes: number): void {
    if (bytes <= 0 || !this.measuring || !this.#live || this.#stopping) return;
    this.#armProgressWatchdog();
    if (this.stalled) this.setStalled(false);
  }

  #armProgressWatchdog(): void {
    if (this.#progressTimer !== null) clearTimeout(this.#progressTimer);
    this.#progressTimer = setTimeout(() => {
      this.#progressTimer = null;
      if (!this.measuring || !this.#live || this.#stopping) return;
      this.setStalled(true, `${this.dir} direction carried no data`);
    }, DIRECTION_PROGRESS_WINDOW_MS);
  }

  /** Flush the partial cadence window, then stop the lanes gracefully: a
   *  session lane finalizes the upload before it is terminated. */
  stop(): Promise<void> {
    return this.#release(true);
  }

  /** Drop the lanes immediately, for an abort or a discarded stage. Discarded
   *  lanes make the release walk a no-op, so nothing is stopped twice. */
  discard(): void {
    for (const lane of this.#lanes) lane?.discard();
    void this.#release(false);
  }

  /** Cancel the timers and release the lanes. `flush` aggregates the partial
   *  cadence window first; a discard has no sample to report. */
  #release(flush: boolean): Promise<void> {
    this.#stopping = true;
    if (this.#progressTimer !== null) clearTimeout(this.#progressTimer);
    this.#progressTimer = null;
    const aggregation = this.#aggregation;
    if (flush && this.measuring && aggregation) this.#aggregate(aggregation);
    if (aggregation?.timer != null) clearInterval(aggregation.timer);
    for (const timer of this.#timers) if (timer) clearTimeout(timer);
    return Promise.all(this.#lanes.map((lane) => lane?.stop())).then(() => {
      this.#stopping = false;
      this.#live = false;
    });
  }

  /** Open (or re-open) lane `i`. A restart builds a fresh lane from the same
   *  factory, so nothing outlives the failure that closed it. */
  #startLane(i: number): void {
    if (!this.#live || this.#stopping) return;
    const lane = this.newLane(i, this.#laneEvents(i, this.#uploadGeneration));
    this.#lanes[i] = lane;
    lane.start();
    // A restarted lane must resume the live measurement window: without the
    // current seq its reports are discarded as pre-measure warmup.
    if (this.measuring && this.dir === "down") lane.measure(this.#measureSeq);
  }

  /** What a lane reports, routed to the same accounting for every transport. */
  #laneEvents(i: number, generation: number): LaneEvents {
    return {
      onProgress: (bytes, elapsedMs, seq) =>
        this.#onProgress(i, bytes, elapsedMs, seq),
      onAlive: (bytes, elapsedMs) =>
        this.#onAlive(i, bytes, elapsedMs, generation),
      onError: (recoverable, detail, cause) =>
        this.#onError(i, detail, recoverable, cause),
      onUploadProgress: (msg) => this.#deps.uploadProgress(msg, generation),
      onAuthRequired: () => {
        this.#deps.discardTransfer();
        redirectToLogin();
      },
    };
  }

  /** Aggregation tick: sum each lane's bytes over that lane's own receive
   *  interval into one real sample, zero-byte windows included. */
  #aggregate(aggregation: ClientByteAggregation): void {
    const now = performance.now();
    const durationSec = (now - aggregation.lastAggregateAt) / 1000;
    // performance.now is coarsened per origin (1ms in Firefox, 100us in
    // Chromium) and #onProgress aggregates per message while stopping, so two
    // reports can land in one tick. A window with no duration has nothing to
    // ingest against: leave its lane bytes pending for the next one.
    if (durationSec <= 0) return;
    aggregation.lastAggregateAt = now;
    let delta = 0;
    let bytesPerSec = 0;
    for (let i = 0; i < aggregation.pendingLaneBytes.length; i++) {
      const laneBytes = aggregation.pendingLaneBytes[i] ?? 0;
      const laneSec = aggregation.pendingLaneElapsedSec[i] ?? 0;
      aggregation.pendingLaneBytes[i] = 0;
      aggregation.pendingLaneElapsedSec[i] = 0;
      if (laneBytes <= 0 || laneSec <= 0) continue;
      delta += laneBytes;
      bytesPerSec += laneBytes / laneSec;
    }
    this.#deps
      .host()
      .ingestThroughput(
        this.dir,
        bytesPerSec,
        delta,
        durationSec,
        false,
        this.#deps.sampleProvesStageLiveness?.() ?? true,
      );
    // Verbose: the pool's combined raw rate at 1 Hz, the sum the core smooths.
    // Compare it to the per-worker logs and the server figure to locate losses.
    if (debugEnabled()) {
      const window = aggregation.debug.add(delta, now);
      if (window) {
        const active = this.#lanes.reduce((n, lane) => n + (lane ? 1 : 0), 0);
        dlog("realrunner:aggregate", `${this.dir} pool`, {
          rate: window.rate,
          tick: fmtRate(bytesPerSec),
          window: window.window,
          streams: active,
          dt: window.dt,
        });
      }
    }
  }

  /** A lane moved bytes. Download reports carry the measure epoch, so warmup
   *  and superseded reports are dropped here rather than in the lane. */
  #onProgress(
    i: number,
    bytes: number,
    elapsedMs?: number,
    seq?: number,
  ): void {
    if (!this.#live) return; // late message after release
    if (
      this.dir === "down" &&
      (seq === undefined || seq !== this.#measureSeq || seq <= 0)
    )
      return;
    this.noteMeasuredProgress(bytes);
    const aggregation = this.#aggregation;
    if (!aggregation) return;
    aggregation.pendingLaneBytes[i] =
      (aggregation.pendingLaneBytes[i] ?? 0) + bytes;
    aggregation.pendingLaneElapsedSec[i] =
      (aggregation.pendingLaneElapsedSec[i] ?? 0) +
      (elapsedMs ?? THROUGHPUT_CADENCE_MS) / 1000;
    if (this.#stopping) this.#aggregate(aggregation);
  }

  /** Upload workers may report local completion timing. It is intentionally not
   * liveness or measurement evidence; only the server feed can establish that. */
  #onAlive(
    lane: number,
    bytes?: number,
    elapsedMs?: number,
    generation?: number,
  ): void {
    if (!this.#live || this.#stopping) return;
    if (
      this.dir === "up" &&
      bytes !== undefined &&
      elapsedMs !== undefined &&
      generation !== undefined
    )
      this.#deps.uploadPresentationHint?.(lane, bytes, elapsedMs, generation);
  }

  /** A lane failed. Recoverable (the common case: a dropped connection) → stall
   *  once, then re-open the lane so a real sample resumes it. */
  #onError(
    i: number,
    detail: string,
    recoverable: boolean,
    cause?: RecoveryCause,
  ): void {
    // Ignore late errors after release (a stop()/terminate races the worker).
    if (!this.#live || this.#stopping) return;
    if (cause === "unknown-upload-id" && this.measuring) {
      // The runner owns the one allowed ID rotation. Do not schedule a same-ID
      // lane restart after the structural refusal: its generation is about to
      // be invalidated and replacement lanes must use only the new ID.
      this.setStalled(true, detail, cause);
      return;
    }
    if (!recoverable) {
      // A refused lane is structural protocol evidence, not a transport stall.
      // Keep any qualifying evidence, end only this stage, and let later
      // configured stages continue through the core's stage contract.
      this.#deps
        .host()
        .failStage(
          this.stage,
          "protocol-error",
          `${this.dir} stream ${i} failed: ${detail}`,
          this.dir,
        );
      return;
    }
    if (this.measuring) this.setStalled(true, detail);
    // Re-open the lane after a backoff, so a persistently failing stream cannot
    // spin a tight respawn loop. A session lane respawns through its owner.
    this.#lanes[i]?.discard();
    this.#lanes[i] = null;
    // One pending restart per lane: an orphan timer would spawn a duplicate
    // worker into the next stage, and the lane would be counted twice.
    const pending = this.#timers[i];
    if (pending) clearTimeout(pending);
    this.#timers[i] = setTimeout(() => {
      this.#timers[i] = null;
      this.#startLane(i);
    }, LANE_RESTART_BACKOFF_MS);
  }
}
