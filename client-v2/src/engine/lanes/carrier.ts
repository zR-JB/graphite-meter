// One direction's carrier for a stage: N fetch lanes, a worker each, or one WebTransport session worker carrying N
// streams. A worker lives as long as its carrier: a lane that ends reopens on it once the stage has settled what the
// ending means, so a session's unspent ticket survives its redials. The carrier is ready when enough lanes delivered
// since they last opened.
import type { FeedRecord, LaneCommand, LaneReport, SessionCommand, SessionReport } from "../../protocol/messages";
import { workerScript } from "../../lib/trusted";
import type { Ending } from "../types";
import { Backoff, lost } from "./endings";
import { fromEpoch } from "../run/clock";
import { delay } from "./wait";

/** A stopping session gets this long to finalize and acknowledge. */
const STOP_GRACE_MS = 2_500;
/** A flush waits at most this long for its lanes' answers. */
const FLUSH_MS = 250;
/** Lanes open at most this far apart and within this spread, so their connections do not start in one burst. */
const STAGGER_MS = 75;
const SPREAD_MS = 1_000;

export interface CarrierHost {
  /** Download bytes of the current measurement epoch. */
  bytes(bytes: number): void;
  posted(lane: number, bytes: number, elapsedMs: number): void;
  record(record: FeedRecord): void;
  /** A lane delivered for the first time since it opened. */
  delivered(): void;
  /** Settles a lane's ending; true when the lane should reopen, after `delayMs`. */
  ended(lane: number, ending: Ending, broken: boolean, delayMs: number, signal: AbortSignal): Promise<boolean>;
}

export interface CarrierPlan {
  worker: string;
  /** Fetch lanes, or one session. */
  lanes: number;
  /** Lanes that must deliver before the carrier is ready. */
  gating: number;
  session: boolean;
  /** A (re)opening's command, with credentials read afresh; null once they ended. */
  open(lane: number, epoch: number): LaneCommand | SessionCommand | null;
}

interface Lane {
  index: number;
  worker: Worker;
  delivered: boolean;
  backoff: Backoff;
  /** Resolves the flush awaiting this lane's answer, with when the lane read its count. */
  flushed: ((at: number) => void) | null;
  /** Answered the final flush: what it counts later lies past the boundary. */
  cut: boolean;
}

export class Carrier {
  readonly #plan: CarrierPlan;
  readonly #host: CarrierHost;
  readonly #lanes: Lane[] = [];
  /** Stopping or released: no lane reopens. */
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

  /** Refused as busy and not delivering since: why the carrier may not be ready. */
  get busy(): boolean {
    return this.#lanes.some((lane) => !lane.delivered && lane.backoff.busy);
  }

  /** A new epoch: download bytes counted before it no longer count. */
  measure(): void {
    this.#epoch++;
    for (const { worker } of this.#lanes) worker.postMessage({ type: "measure", epoch: this.#epoch });
  }

  /** Resolves once every lane reported the download bytes it counted so far, within 250 ms, with the latest page
      time a lane read its count at, or null when none answered. A lane that answered counts nothing more. */
  async flush(): Promise<number | null> {
    const answered = new AbortController();
    let latest: number | null = null;
    const answers = this.#lanes.map(
      (lane) =>
        new Promise<void>((resolve) => {
          lane.flushed = (at) => {
            lane.cut = true;
            latest = Math.max(latest ?? Number.NEGATIVE_INFINITY, fromEpoch(at));
            resolve();
          };
          lane.worker.postMessage({ type: "flush" });
        }),
    );
    await Promise.race([Promise.all(answers), delay(FLUSH_MS, AbortSignal.any([this.#done.signal, answered.signal]))]);
    answered.abort();
    return latest;
  }

  /** A session finalizes and acknowledges within its grace; fetch lanes end at once. */
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
        if (report.epoch === this.#epoch && !lane.cut) this.#host.bytes(report.bytes);
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
        lane.flushed?.(report.at);
        lane.flushed = null;
        return;
      case "ended":
        return void this.#ended(lane, report.ending);
      case "broken":
        return void this.#ended(lane, lost(report.detail), true);
    }
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
