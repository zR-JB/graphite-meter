// One server's resources for one stage; a failure or ended sign-in reaches the run once.
import type { LaneCommand, SessionCommand } from "../../protocol/messages";
import { ROUTES } from "../../protocol/routes";
import { feedSocket, needsPings } from "../net/streams";
import {
  CADENCE_MS,
  SignInRequired,
  StageFailure,
  type Checkpoint,
  type Direction,
  type Ending,
  type FailureReason,
  type OpenStage,
  type PreparedServer,
  type LatencyBus,
  type ProbeChannelEvents,
  type Role,
  type StageResources,
  type StageSetup,
  type Stall,
} from "../types";
import { Carrier, type CarrierHost, type CarrierPlan } from "./carrier";
import { answered, ESTABLISH_MS } from "./endings";
import { handshakes, THROTTLED } from "../net/webtransport";
import { Receiver, type ReceiverHost } from "./receiver";
import { abortable, delay } from "../run/clock";

/** Readiness gets a dial's budget and a margin for the workers to report within it. */
const READY_MS = ESTABLISH_MS + 500;
/** Large enough that only the server's clock or a stop ends a download. */
const DOWNLOAD_BYTES = 64 * 1024 ** 3;
/** Download bodies end this long after the stage's planned end, so its last window still sees bytes flow. */
const END_MARGIN_MS = 250;
/** Bounds the probes in flight. A loaded stage shares the link with its transfer, so its window stays small. */
const IN_FLIGHT = { fixed: 16, replyDriven: 4, loaded: 2 };
/** One sign-in check answers every break this long after it started, so an outage is not checked per reopening. */
const SIGN_IN_CHECK_MS = 3_000;

export const openStage: OpenStage = (setup) => new Stage(setup);

/** An upload id minted with the server's lease; a broken request asks `signedOut` whether the sign-in ended. */
export async function mintUpload(
  { link, server }: PreparedServer,
  signal: AbortSignal,
  signedOut: () => Promise<boolean> = async () => false,
): Promise<string> {
  try {
    return await link.lease.upload(AbortSignal.any([signal, AbortSignal.timeout(ESTABLISH_MS)]));
  } catch (cause) {
    if (cause instanceof SignInRequired || signal.aborted) throw cause;
    // A broken request may be the sign-in or grant ending. The check, too, ends with the budget.
    if (await abortable(signedOut(), signal)) throw new SignInRequired(server);
    throw new Error("the upload id could not be minted", { cause });
  }
}

class Stage implements StageResources {
  readonly #setup: StageSetup;
  /** Nothing reaches the run once the stage is released. */
  readonly #released = new AbortController();
  #measuring = false;
  /** Throughput failed or its sign-in ended, so its carriers are gone for good. */
  #throughputEnded = false;
  #latencyEnded = false;
  #down: Carrier | null = null;
  #up: Carrier | null = null;
  #receiver: Receiver | null = null;
  #probes: LatencyBus | null = null;
  #probesReady = false;
  #probesMeasuring = false;
  readonly #stalls: Record<Direction, boolean> = { down: false, up: false };
  #unprepared: ((error: Error) => void) | null = null;
  #changed = Promise.withResolvers<void>();
  #signInCheck: { at: number; ended: Promise<boolean> } | null = null;
  /** Lanes and feeds that reopened once already; a second ending other than a server bound fails the stage. */
  readonly #reopened = new Set<string>();
  /** The run's clock runs for this stage, so its planned end is exact. */
  #planned = false;
  #uploadId: Promise<string | null>;

  constructor(setup: StageSetup) {
    this.#setup = setup;
    this.#uploadId = setup.uploadId;
  }

  async prepare(signal: AbortSignal): Promise<void> {
    const budget = AbortSignal.any([signal, this.#released.signal, AbortSignal.timeout(READY_MS)]);
    const failed = new Promise<never>((_, reject) => (this.#unprepared = reject));
    const { stage, transfer } = this.#setup.activity;
    try {
      this.#openProbes();
      if (transfer.includes("down")) this.#down = this.#carrier("down");
      await Promise.race([
        failed,
        Promise.all([
          this.#down && this.#until(() => this.#down!.ready, budget),
          transfer.includes("up") &&
            this.#prepareUpload(budget).then((receiver) => receiver.accepted(budget)),
        ]),
      ]);
      const probing =
        this.#probes && this.#until(() => this.#probesReady || this.#latencyEnded, budget);
      // Loaded latency never holds throughput back. Its channel reports its own stall.
      if (probing)
        await Promise.race([
          failed,
          stage === "latency"
            ? probing
            : probing.catch(() => {
                signal.throwIfAborted();
                this.#released.signal.throwIfAborted();
              }),
        ]);
    } catch (error) {
      throw this.#unready(error, signal);
    } finally {
      this.#unprepared = null;
    }
  }

  measure(at: number): void {
    this.#measuring = true;
    this.#down?.measure(at);
    this.#receiver?.measure();
    this.#probesMeasuring = !!this.#probes;
    this.#probes?.measure();
  }

  replanned(): void {
    this.#planned = true;
    if (this.#timed) this.#down?.ends(this.#bodiesEnd()!);
  }

  get #timed(): boolean {
    const { protocol } = this.#setup.server.throughput.fetch;
    return protocol === "http2" || protocol === "http3";
  }

  /** Epoch ms download bodies end by, just past the planned end so the window's last bytes flow; null while the
   *  clock holds for the stage to prepare, as its end is not known yet. */
  #bodiesEnd(): number | null {
    if (!this.#planned) return null;
    return performance.timeOrigin + performance.now() + this.#setup.endsIn() + END_MARGIN_MS;
  }

  async checkpoint(signal: AbortSignal, final = false): Promise<Checkpoint | null> {
    signal.throwIfAborted();
    return this.#receiver ? this.#receiver.checkpoint(signal, final) : null;
  }

  async countAt(at: number): Promise<number | null> {
    return (await this.#down?.countAt(at)) ?? null;
  }

  async flush(cutoff: number): Promise<number | null> {
    return (await this.#down?.flush(cutoff)) ?? null;
  }

  async replaceUpload(signal: AbortSignal): Promise<void> {
    if (!this.#receiver || this.#throughputEnded || this.#released.signal.aborted) return;
    this.#receiver.close();
    const released = this.#up?.release();
    this.#receiver = this.#up = null;
    // The replacement's lanes and feed start with their own reopen.
    for (const label of this.#reopened) if (label.startsWith("up")) this.#reopened.delete(label);
    await released;
    if (signal.aborted || this.#released.signal.aborted) return;
    try {
      await this.#prepareUpload(
        AbortSignal.any([signal, this.#released.signal, AbortSignal.timeout(READY_MS)]),
      );
    } catch (error) {
      if (signal.aborted || this.#released.signal.aborted) return;
      if (error instanceof SignInRequired) this.#signIn("throughput");
      else this.#fail("path-failed", error instanceof Error ? error.message : String(error));
    }
  }

  async finish(): Promise<void> {
    if (this.#released.signal.aborted) return;
    const drained = this.#probes?.stop().then(() => (this.#probesMeasuring = false));
    await Promise.all([this.#down?.stop(), this.#up?.stop()]);
    await this.#receiver?.finish();
    await drained;
    // The bus outlives the stage, for the next one to adopt.
    this.#probes = null;
    this.discard();
  }

  discard(incomplete = false): void {
    if (this.#released.signal.aborted) return;
    if (this.#probes) {
      // The bus stays the server's; outcomes it still owes this population are lost to it.
      if (incomplete && this.#probesMeasuring) this.#setup.events.probesIncomplete();
      this.#setup.server.link.bus.park();
    }
    this.#released.abort();
    this.#throughputEnded = true;
    this.#releaseThroughput();
  }

  /** Why the stage is not ready: an abort as such, a failure as reported, a lapsed budget as a path that failed. */
  #unready(error: unknown, signal: AbortSignal): unknown {
    if (signal.aborted) return signal.reason;
    if (!(error instanceof DOMException && error.name === "TimeoutError")) return error;
    return new StageFailure(
      "path-failed",
      `Connections did not become ready within ${READY_MS / 1000} s`,
    );
  }

  async #until(test: () => boolean, signal: AbortSignal): Promise<void> {
    while (!test()) await abortable(this.#changed.promise, signal);
  }

  #notify(): void {
    this.#changed.resolve();
    this.#changed = Promise.withResolvers();
  }

  #openProbes(): void {
    const { server, activity, cadence, events } = this.#setup;
    const { bus } = server.link;
    if (!server.latency || !needsPings(activity)) return;
    const intervalMs = CADENCE_MS[cadence];
    const maxInFlight =
      activity.stage !== "latency"
        ? IN_FLIGHT.loaded
        : intervalMs === null
          ? IN_FLIGHT.replyDriven
          : IN_FLIGHT.fixed;
    const own: ProbeChannelEvents = {
      ready: () => {
        this.#probesReady = true;
        this.#notify();
      },
      outcomes: (outcomes) => events.probes(outcomes),
      unmeasured: (rtts) => events.warmupReplies(rtts),
      interrupted: (count, reason) => events.probesInterrupted(count, reason),
      stalled: (detail) => {
        this.#probesReady = false;
        events.latencyStalled(detail);
      },
      resumed: () => events.latencyResumed(),
      signInRequired: () => this.#signIn("latency"),
      incomplete: () => events.probesIncomplete(),
    };
    const { target } = server.latency;
    // A handshake behind a stage's own transfers can take seconds, so a stage adopts the server's bus already open.
    const adopted = bus.adopt(target, own);
    adopted?.pace(intervalMs, maxInFlight);
    if (!adopted && target.transport !== "websocket" && handshakes.throttled.peek()) {
      // Loaded latency never holds throughput back.
      if (activity.stage === "latency") throw new StageFailure("path-failed", THROTTLED);
      return events.latencyStalled(THROTTLED);
    }
    this.#probes =
      adopted ??
      bus.take(
        { target, auth: server.auth, control: server.server.url, intervalMs, maxInFlight },
        own,
      );
    // The latency warmup shows its replies before its window counts any.
    if (activity.stage === "latency") this.#probes.warm();
  }

  /** Mints an upload id and opens its receiver; HTTP lanes write only once the feed is open, so it reports from the first byte. */
  async #prepareUpload(signal: AbortSignal): Promise<Receiver> {
    const { auth, discovery, server, throughput } = this.#setup.server;
    // The run's id serves the stage's first upload; a replacement is minted.
    const minted = this.#uploadId;
    this.#uploadId = Promise.resolve(null);
    const id =
      (await abortable(minted, signal)) ??
      (await mintUpload(this.#setup.server, signal, () => this.#signedOut()));
    if (this.#throughputEnded) throw new Error("throughput already ended");
    const carried = throughput.target.transport !== "fetch-stream";
    const route = carried ? "session" : feedSocket(discovery) ? "websocket" : "http";
    // The lease owns the upload, so its control goes to the discovery origin, whichever origin carries its lanes.
    const receiver = new Receiver(this.#receiverHost, auth, server.url, id, route);
    this.#receiver = receiver;
    if (this.#measuring) receiver.measure();
    if (carried) this.#up = this.#carrier("up", receiver);
    await receiver.opened(signal);
    if (!carried) this.#up = this.#carrier("up", receiver);
    return receiver;
  }

  /** Opens one direction's carrier and its lanes now, or none once throughput ended. */
  #carrier(direction: Direction, receiver?: Receiver): Carrier | null {
    if (this.#throughputEnded) return null;
    const { server, streams, uploadLanes, gating } = this.#setup;
    const { auth } = server;
    const { target, fetch } = server.throughput;
    const count = streams[direction];
    const upload = receiver ? `id=${encodeURIComponent(receiver.id)}` : "";
    let plan: CarrierPlan;
    if (target.transport === "fetch-stream") {
      plan = {
        pool: server.link.workers,
        worker: WORKERS.lane,
        lanes: count,
        gating: direction === "down" ? Math.min(count, gating?.down ?? count) : 0,
        session: false,
        open: (_, epoch) =>
          this.#signedIn((): LaneCommand => {
            if (direction === "up") {
              const url = `${fetch.origin}${ROUTES.upload}?${upload}`;
              const rttMs = server.latency?.rttMs ?? null;
              const request = auth.request(url, "POST");
              return { type: "upload", url, request, streams: count, sharing: uploadLanes, rttMs };
            }
            const url = `${fetch.origin}${ROUTES.download}?bytes=${DOWNLOAD_BYTES}`;
            return {
              type: "download",
              url,
              request: auth.request(url, "GET"),
              epoch,
              streams: count,
              timed: this.#timed,
              until: this.#timed ? this.#bodiesEnd() : null,
            };
          }),
      };
    } else {
      const datagrams = target.transport === "webtransport-datagram";
      const route = direction === "down" ? ROUTES.wtDownload : ROUTES.wtUpload;
      const query =
        direction === "down"
          ? `bytes=${DOWNLOAD_BYTES}&${datagrams ? "datagrams=1" : `streams=${count}`}`
          : upload;
      const url = `${target.origin}${route}?${query}${direction === "up" && datagrams ? "&datagrams=1" : ""}`;
      plan = {
        pool: server.link.workers,
        worker: WORKERS.session,
        lanes: 1,
        gating: direction === "down" ? 1 : 0,
        session: true,
        open: (_, epoch) =>
          this.#signedIn((): SessionCommand => {
            if (handshakes.throttled.peek()) throw new Error(THROTTLED);
            const leased = auth.socket(url);
            if (direction === "down") return { type: "download", url: leased, datagrams, epoch };
            const finalize = {
              url: receiver!.feed,
              request: auth.request(receiver!.feed, "DELETE"),
            };
            return { type: "upload", url: leased, datagrams, streams: count, finalize };
          }),
      };
    }
    return new Carrier(plan, this.#carrierHost(direction));
  }

  /** Reads a request's credentials afresh at every opening; an ended grant asks for sign-in, any other refusal fails the stage. */
  #signedIn<T>(build: () => T): T | null {
    try {
      return build();
    } catch (error) {
      if (error instanceof SignInRequired) this.#signIn("throughput");
      else this.#fail("path-failed", error instanceof Error ? error.message : String(error));
      return null;
    }
  }

  #carrierHost(direction: Direction): CarrierHost {
    const { events } = this.#setup;
    return {
      bytes: (bytes) => {
        if (this.#throughputEnded) return;
        if (this.#measuring) events.download(bytes);
        else events.warmupDownload(bytes);
      },
      posted: (lane, bytes, elapsedMs) => {
        if (!this.#throughputEnded) events.uploadHint(lane, bytes, elapsedMs);
      },
      record: (record) => this.#receiver?.record(record),
      // Only a lane that reopened proves a download moves again: bytes read before its own break arrive after others'.
      delivered: () => {
        this.#notify();
        if (direction === "down") this.#progress("down");
      },
      ended: (lane, ending, broken, delayMs, signal) =>
        this.#reopen(direction, `${direction} stream ${lane}`, ending, broken, delayMs, signal),
    };
  }

  readonly #receiverHost: ReceiverHost = {
    now: () => this.#setup.now(),
    observed: (checkpoint, grew) => {
      if (this.#throughputEnded) return;
      if (this.#measuring) this.#setup.events.receiver(checkpoint);
      else this.#setup.events.warmupReceiver(checkpoint);
      if (grew) this.#progress("up");
    },
    ended: (ending, broken, delayMs, signal) =>
      this.#reopen("up", "upload progress", ending, broken, delayMs, signal),
    signIn: () => this.#signIn("throughput"),
  };

  /** Settles a lane's or the feed's ending: one reopen once the server answers over HTTP, within 3 s; checking the sign-in after a break may outlast the silence rule. */
  async #reopen(
    direction: Direction,
    label: string,
    ending: Ending,
    broken: boolean,
    delayMs: number,
    signal: AbortSignal,
  ): Promise<boolean> {
    if (this.#throughputEnded || signal.aborted) return false;
    if (ending.reason === "sign-in-required") {
      this.#signIn("throughput");
      return false;
    }
    const stall: Stall = { reason: ending.reason, detail: ending.detail, direction };
    // The run owns the one replacement of an upload id the server no longer knows.
    if (ending.rotate && this.#measuring) {
      this.#stall(direction, { ...stall, rotate: true });
      return false;
    }
    if (!ending.retry || (!ending.bound && this.#reopened.has(label))) {
      this.#fail(ending.reason, `${label} failed: ${ending.detail}`);
      return false;
    }
    if (!ending.bound) this.#reopened.add(label);
    this.#stall(direction, stall);
    if (broken) void this.#signedOut().then((signedOut) => signedOut && this.#signIn("throughput"));
    await delay(delayMs, signal);
    const { auth, server } = this.#setup.server;
    const answers = await answered(server.url, auth, signal, ESTABLISH_MS);
    // Whoever closed this lane or feed meanwhile owns its ending.
    if (signal.aborted || this.#throughputEnded) return false;
    if (!answers)
      this.#fail(
        "connection-lost",
        `${label} failed: ${ending.detail}; the server did not answer over HTTP`,
      );
    return answers;
  }

  /** Asks whether the sign-in or grant ended. A revocation mid-outage still shows within one more check. */
  #signedOut(): Promise<boolean> {
    const at = this.#setup.now();
    if (this.#signInCheck && at - this.#signInCheck.at < SIGN_IN_CHECK_MS)
      return this.#signInCheck.ended;
    const ended = this.#setup.server.auth.ended(this.#released.signal).catch(() => false);
    this.#signInCheck = { at, ended };
    return ended;
  }

  /** The stage stalls once while any direction does. A rotation reaches the run even then. */
  #stall(direction: Direction, stall: Stall): void {
    if (!this.#measuring || (this.#stalls[direction] && !stall.rotate)) return;
    const quiet = !this.#stalls.down && !this.#stalls.up;
    this.#stalls[direction] = true;
    if (quiet || stall.rotate) this.#setup.events.stalled(stall);
  }

  /** A download lane delivering since it reopened, or receiver growth, clears its direction's stall. */
  #progress(direction: Direction): void {
    if (!this.#stalls[direction]) return;
    this.#stalls[direction] = false;
    if (!this.#stalls.down && !this.#stalls.up) this.#setup.events.resumed();
  }

  #signIn(role: Role): void {
    if (
      this.#released.signal.aborted ||
      (role === "throughput" ? this.#throughputEnded : this.#latencyEnded)
    )
      return;
    if (role === "throughput") {
      this.#throughputEnded = true;
      this.#releaseThroughput();
    } else this.#latencyEnded = true;
    this.#notify();
    // Loaded latency alone never ends the stage, so it never fails a preparation.
    if (this.#unprepared && (role === "throughput" || this.#setup.activity.stage === "latency"))
      this.#unprepared(new SignInRequired(this.#setup.server.server));
    else this.#setup.events.signInRequired(role);
  }

  #fail(reason: FailureReason, message: string): void {
    if (this.#released.signal.aborted || this.#throughputEnded) return;
    this.#throughputEnded = true;
    this.#releaseThroughput();
    if (this.#unprepared) this.#unprepared(new StageFailure(reason, message));
    else this.#setup.events.failed(reason, message);
  }

  #releaseThroughput(): void {
    this.#down?.release();
    this.#up?.release();
    this.#receiver?.close();
  }
}
