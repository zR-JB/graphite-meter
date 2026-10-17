// Carries one stage direction over fetch lanes, a worker each, or one WebTransport session worker. A worker lives as
// long as its carrier, so a lane that ends reopens on it and a session's unspent ticket survives its redials.
import type {
  FeedRecord,
  Flushed,
  LaneCommand,
  LaneReport,
  SessionCommand,
  SessionReport,
} from "../../protocol/messages";
import { workerScript } from "../../lib/trusted";
import type { Ending } from "../types";
import { delay, fromEpoch } from "../run/clock";
import { Backoff, lost } from "./endings";

/** A stopping session gets this long to finalize and acknowledge. */
const STOP_GRACE_MS = 2_500;
/** A flush waits at most this long for its lanes' answers. */
const FLUSH_MS = 250;
/** Lanes open at most this far apart and within this spread, so their connections do not start in one burst. */
const STAGGER_MS = 75;
const SPREAD_MS = 1_000;

export interface CarrierHost {
  /** Counts download bytes of the current measurement epoch. */
  bytes(bytes: number): void;
  posted(lane: number, bytes: number, elapsedMs: number): void;
  record(record: FeedRecord): void;
  /** A lane delivered for the first time since it opened. */
  delivered(): void;
  /** Settles a lane's ending. Returns true when the lane should reopen, after `delayMs`. */
  ended(lane: number, ending: Ending, broken: boolean, delayMs: number, signal: AbortSignal): Promise<boolean>;
}

export interface CarrierPlan {
  worker: string;
  /** Fetch lanes, or one session. */
  lanes: number;
  /** Lanes that must deliver before the carrier is ready. */
  gating: number;
  session: boolean;
  /** Builds a (re)opening's command with credentials read afresh, or null once they ended. */
  open(lane: number, epoch: number): LaneCommand | SessionCommand | null;
}

interface Lane {
  index: number;
  worker: Worker;
  delivered: boolean;
  backoff: Backoff;
  /** Download bytes of the current epoch passed on to the host. */
  counted: number;
  /** Bytes reported while a flush awaits this lane's answer, which counts them itself. Null when none awaits. */
  held: number | null;
  /** Resolves the flush awaiting this lane's answer. */
  flushed: ((answer: Flushed) => void) | null;
  /** The lane answered the final flush, so what it counts later lies past the boundary. */
  cut: boolean;
}

export class Carrier {
  readonly #plan: CarrierPlan;
  readonly #host: CarrierHost;
  readonly #lanes: Lane[] = [];
  /** No lane reopens once the carrier is stopping or released. */
  readonly #done = new AbortController();
  readonly #acknowledged = new AbortController();
  #epoch = 0;

  constructor(plan: CarrierPlan, host: CarrierHost) {
    this.#plan = plan;
    this.#host = host;
    const stagger = plan.lanes > 1 ? Math.min(STAGGER_MS, Math.floor(SPREAD_MS / (plan.lanes - 1))) : 0;
    for (let index = 0; index < plan.lanes; index++) {
      const lane: Lane = {
        index,
        worker: new Worker(workerScript(plan.worker), { type: "module" }),
        delivered: false,
        backoff: new Backoff(),
        counted: 0,
        held: null,
        flushed: null,
        cut: false,
      };
      lane.worker.onmessage = ({ data }: MessageEvent<LaneReport | SessionReport>) => this.#report(lane, data);
      // Our workers report every ending they can, so an error event means one cannot run at all.
      lane.worker.onerror = ({ message }) =>
        void this.#ended(lane, { reason: "connection-lost", retry: false, detail: `worker failed: ${message}` });
      this.#lanes.push(lane);
      if (index) void delay(index * stagger, this.#done.signal).then(() => this.#open(lane));
      else this.#open(lane);
    }
  }

  get ready(): boolean {
    let delivered = 0;
    for (const lane of this.#lanes) if (lane.delivered) delivered++;
    return delivered >= this.#plan.gating;
  }

  /** A lane refused as busy that has not delivered since explains why the carrier may not be ready. */
  get busy(): boolean {
    return this.#lanes.some((lane) => !lane.delivered && lane.backoff.busy);
  }

  /** Starts a new epoch, so download bytes counted before it no longer count. */
  measure(): void {
    this.#epoch++;
    for (const lane of this.#lanes) {
      lane.counted = 0;
      lane.worker.postMessage({ type: "measure", epoch: this.#epoch });
    }
  }

  /** Resolves once every lane answered the download bytes it had counted by `cutoff` (page time), within 250 ms,
      with the latest page time those counts stand at. That is the cutoff, unless a lane no longer kept it, or now for
      a lane that did not answer and counts on. Null without lanes. The rate a boundary there reads never overstates. */
  async flush(cutoff: number): Promise<number | null> {
    const answered = new AbortController();
    let latest: number | null = null;
    const standsAt = (at: number) => (latest = Math.max(latest ?? Number.NEGATIVE_INFINITY, at));
    const answers = this.#lanes.map(
      (lane) =>
        new Promise<void>((resolve) => {
          lane.held = 0;
          lane.flushed = ({ total, at }) => {
            lane.cut = true;
            lane.held = null;
            if (total > lane.counted) this.#count(lane, total - lane.counted);
            standsAt(fromEpoch(at));
            resolve();
          };
          lane.worker.postMessage({ type: "flush", at: performance.timeOrigin + cutoff });
        }),
    );
    await Promise.race([Promise.all(answers), delay(FLUSH_MS, AbortSignal.any([this.#done.signal, answered.signal]))]);
    answered.abort();
    for (const lane of this.#lanes) {
      if (lane.flushed) {
        if (lane.held) this.#count(lane, lane.held);
        standsAt(performance.now());
      }
      lane.held = lane.flushed = null;
    }
    return latest;
  }

  /** A session finalizes and acknowledges within its grace. Fetch lanes end at once. */
  async stop(): Promise<void> {
    this.#done.abort();
    const session = this.#plan.session && this.#lanes[0];
    if (session) {
      session.worker.postMessage({ type: "stop" } satisfies SessionCommand);
      await delay(STOP_GRACE_MS, this.#acknowledged.signal);
    }
    this.release();
  }

  release(): void {
    this.#done.abort();
    this.#acknowledged.abort();
    for (const { worker } of this.#lanes.splice(0)) {
      worker.onmessage = worker.onerror = null;
      worker.terminate();
    }
  }

  #open(lane: Lane): void {
    if (this.#done.signal.aborted) return;
    const command = this.#plan.open(lane.index, this.#epoch);
    if (command) lane.worker.postMessage(command);
  }

  #report(lane: Lane, report: LaneReport | SessionReport): void {
    switch (report.type) {
      case "bytes":
        this.#delivered(lane);
        if (report.epoch !== this.#epoch || lane.cut) return;
        if (lane.held === null) this.#count(lane, report.bytes);
        else lane.held += report.bytes;
        return;
      case "posted":
        this.#delivered(lane);
        return this.#host.posted(lane.index, report.bytes, report.elapsedMs);
      case "record":
        this.#delivered(lane);
        return this.#host.record(report.record);
      case "stopped":
        return this.#acknowledged.abort();
      case "flushed":
        lane.flushed?.(report);
        lane.flushed = null;
        return;
      case "ended":
        return void this.#ended(lane, report.ending);
      case "broken":
        return void this.#ended(lane, lost(report.detail), true);
    }
  }

  #count(lane: Lane, bytes: number): void {
    lane.counted += bytes;
    this.#host.bytes(bytes);
  }

  #delivered(lane: Lane): void {
    lane.backoff.reset();
    if (lane.delivered) return;
    lane.delivered = true;
    this.#host.delivered();
  }

  async #ended(lane: Lane, ending: Ending, broken = false): Promise<void> {
    lane.delivered = false;
    if (await this.#host.ended(lane.index, ending, broken, lane.backoff.after(ending), this.#done.signal))
      this.#open(lane);
  }
}
