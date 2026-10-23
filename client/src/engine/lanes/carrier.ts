// One stage direction over fetch lanes, a worker each, or one WebTransport session worker that outlives its redials.
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
import { handshakes } from "../net/webtransport";

/** A stopping session gets this long to finalize and acknowledge. */
const STOP_GRACE_MS = 2_500;
/** A flush waits at most this long for its lanes' answers. */
const FLUSH_MS = 250;
/** Lanes open at most this far apart and within this spread, so their connections do not start in one burst. */
const STAGGER_MS = 75;
const SPREAD_MS = 1_000;

export interface CarrierHost {
  bytes(bytes: number): void;
  posted(lane: number, bytes: number, elapsedMs: number): void;
  record(record: FeedRecord): void;
  /** A lane delivered for the first time since it opened. */
  delivered(): void;
  /** Settles a lane's ending. Returns true when the lane should reopen, after `delayMs`. */
  ended(
    lane: number,
    ending: Ending,
    broken: boolean,
    delayMs: number,
    signal: AbortSignal,
  ): Promise<boolean>;
}

export interface CarrierPlan {
  worker: string;
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
  /** Bytes reported while the final flush awaits this lane's answer, which counts them itself. Null when none awaits. */
  held: number | null;
  /** Handlers of the count answers asked for, in the order the worker answers them. */
  asked: ((answer: Flushed) => void)[];
  /** The lane answered the final flush, so what it counts later lies past the boundary. */
  cut: boolean;
  /** A session's WebTransport dial is out, so ending the worker now abandons it, which Chrome counts as a failure. */
  dialing: boolean;
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
    const stagger =
      plan.lanes > 1 ? Math.min(STAGGER_MS, Math.floor(SPREAD_MS / (plan.lanes - 1))) : 0;
    for (let index = 0; index < plan.lanes; index++) {
      const lane: Lane = {
        index,
        worker: new Worker(workerScript(plan.worker), { type: "module" }),
        delivered: false,
        backoff: new Backoff(),
        counted: 0,
        held: null,
        asked: [],
        cut: false,
        dialing: false,
      };
      lane.worker.onmessage = ({ data }: MessageEvent<LaneReport | SessionReport>) =>
        this.#report(lane, data);
      // Our workers report every ending they can, so an error event means one cannot run at all.
      lane.worker.onerror = ({ message }) =>
        void this.#ended(lane, {
          reason: "connection-lost",
          retry: false,
          detail: `worker failed: ${message}`,
        });
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

  /** Starts a new epoch at page time `at`: download bytes counted before it no longer count. */
  measure(at: number): void {
    this.#epoch++;
    for (const lane of this.#lanes) {
      lane.counted = 0;
      lane.worker.postMessage({
        type: "measure",
        epoch: this.#epoch,
        at: performance.timeOrigin + at,
      });
    }
  }

  /** The epoch's download bytes every lane counted by `at`, or null when one does not answer in time. */
  async countAt(at: number): Promise<number | null> {
    if (this.#done.signal.aborted) return null;
    let total = 0;
    return (await this.#ask(at, (_, answer) => (total += answer.total))) ? total : null;
  }

  /** Collects every lane's download count at `cutoff` and resolves with the latest time they stand at, so a rate never overstates. */
  async flush(cutoff: number): Promise<number | null> {
    let latest: number | null = null;
    const standsAt = (at: number) => (latest = Math.max(latest ?? Number.NEGATIVE_INFINITY, at));
    for (const lane of this.#lanes) lane.held = 0;
    await this.#ask(cutoff, (lane, { total, at }) => {
      // Past FLUSH_MS the lane's count is settled without its answer.
      if (lane.held === null) return;
      lane.cut = true;
      lane.held = null;
      if (total > lane.counted) this.#count(lane, total - lane.counted);
      standsAt(fromEpoch(at));
    });
    for (const lane of this.#lanes) {
      if (lane.held === null) continue;
      if (lane.held) this.#count(lane, lane.held);
      lane.held = null;
      standsAt(performance.now());
    }
    return latest;
  }

  /** Asks every lane for its count at page time `at`; true once all answered within FLUSH_MS. */
  async #ask(at: number, answer: (lane: Lane, flushed: Flushed) => void): Promise<boolean> {
    const answered = new AbortController();
    const answers = this.#lanes.map(
      (lane) =>
        new Promise<void>((resolve) => {
          lane.asked.push((flushed) => {
            answer(lane, flushed);
            resolve();
          });
          lane.worker.postMessage({ type: "flush", at: performance.timeOrigin + at });
        }),
    );
    const all = await Promise.race([
      Promise.all(answers).then(() => true),
      delay(FLUSH_MS, AbortSignal.any([this.#done.signal, answered.signal])).then(() => false),
    ]);
    answered.abort();
    return all;
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
    for (const { worker, dialing } of this.#lanes.splice(0)) {
      if (dialing) handshakes.failed();
      worker.onmessage = worker.onerror = null;
      worker.terminate();
    }
  }

  #open(lane: Lane): void {
    if (this.#done.signal.aborted) return;
    const command = this.#plan.open(lane.index, this.#epoch);
    if (!command) return;
    lane.dialing = this.#plan.session;
    lane.worker.postMessage(command);
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
      case "handshake":
        lane.dialing = false;
        return void (report.opened || handshakes.failed());
      case "flushed":
        return lane.asked.shift()?.(report);
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
    // A session that delivers has opened, whether or not its report came first.
    lane.dialing = false;
    lane.backoff.reset();
    if (lane.delivered) return;
    lane.delivered = true;
    this.#host.delivered();
  }

  async #ended(lane: Lane, ending: Ending, broken = false): Promise<void> {
    lane.delivered = false;
    if (
      await this.#host.ended(
        lane.index,
        ending,
        broken,
        lane.backoff.after(ending),
        this.#done.signal,
      )
    )
      this.#open(lane);
  }
}
