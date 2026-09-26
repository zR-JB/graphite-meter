// One owner per server stage: its byte lanes, upload receiver and ping channel.
import type {
  ConnectionRole,
  FlowDirection,
  LatencyObservation,
  PhaseActivity,
  PreparedPaths,
  ReceiverCheckpoint,
  RecoveryCause,
  RunnerConfig,
  StallInfo,
} from "./contract";
import { readJSONResponse, parseResponseToken } from "../api/decode";
import {
  classifyServerAuthentication,
  measurementFetch,
  reportServerAuthentication,
  requestOptions,
  socketMint,
  type ServerCredentials,
} from "../servers/credentials";
import {
  authenticationRequired,
  redirectForCredentials,
  sessionAuthenticationRequired,
} from "../request-auth";
import { abortable, abortableDelay } from "./abortable";
import {
  laneStaggerMs,
  laneUrl,
  needsPings,
  PER_STREAM_BYTES,
  ROUTES,
} from "./paths";
import {
  DIRECTION_PROGRESS_WINDOW_MS,
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
  LANE_RESTART_BACKOFF_MS,
  PROGRESS_FINAL_GRACE_MS,
  STOP_GRACE_MS,
} from "./real/budgets";
import { LatencyChannel } from "./real/latencyChannel";
import {
  classifyUploadFailure,
  readProgressFeed,
  type ProgressEvent,
} from "./workers/progressFeed";

/** What a server's stage resources report to the run that owns them. */
export interface ParticipantHost {
  readonly config: RunnerConfig;
  /** Milliseconds on the run's timeline, which saved evidence shares. */
  now(): number;
  /** Measured client-consumed download bytes. */
  download(bytes: number): void;
  /** Upload receiver evidence, pushed by its feed or answered by a checkpoint. */
  receiver(checkpoint: ReceiverCheckpoint): void;
  latency(sample: LatencyObservation): void;
  latencyInterrupted(count: number, reason: "unresolved" | "send-failed"): void;
  latencyIncomplete(): void;
  stall(info: StallInfo): void;
  resume(): void;
  stallLatency(detail: string): void;
  resumeLatency(): void;
  /** A terminal failure of this server's current stage resources. */
  fail(reason: string, message: string): void;
  authenticationRequired(role: ConnectionRole): void;
  /** A locally timed upload completion, for presentation only. */
  uploadHint(lane: number, bytes: number, elapsedMs: number): void;
}

export interface StageTransport {
  /** Opens and primes every connection the activity names. */
  prepare(): Promise<void>;
  /** Every primed channel works, including fresh upload receiver evidence. */
  ready(signal: AbortSignal): Promise<void>;
  measure(): void;
  /** Graceful end: final probe outcomes and lane counters, then release. */
  finish(): Promise<void>;
  /** Immediate release; `incomplete` reports unknown probe outcomes first. */
  discard(incomplete?: boolean): void;
  /** `fresh` issues a request at or after this call instead of joining one in flight. */
  checkpoint(
    signal: AbortSignal,
    fresh?: boolean,
  ): Promise<ReceiverCheckpoint | null>;
  replaceUpload?(signal: AbortSignal): Promise<void>;
}

export interface StageOptions {
  host: ParticipantHost;
  paths: PreparedPaths;
  activity: PhaseActivity;
  streams: Record<FlowDirection, number>;
  seed: string;
}

const LANE_STAGGER_MS = 75;
const CHECKPOINT_TIMEOUT_MS = 1500;

export type WorkerMsg =
  | { type: "established" | "stopped" | "auth-required" }
  | { type: "progress"; bytes: number; elapsedMs?: number; seq?: number }
  | { type: "alive"; bytes?: number; elapsedMs?: number }
  | {
      type: "error";
      recoverable: boolean;
      detail: string;
      cause?: RecoveryCause;
    }
  | { type: "upload-progress"; msg: ProgressEvent };

export interface Lane {
  measure(seq: number): void;
  stop(): Promise<void>;
  discard(): void;
}

/** One worker per fetch lane, or one per WebTransport session; a discarded worker never reaches its owner. */
export function openLane(
  worker: Worker,
  start: object,
  session: boolean,
  on: (msg: WorkerMsg) => void,
): Lane {
  let failed = false;
  let stopped: (() => void) | null = null;
  const fail = (msg: Extract<WorkerMsg, { type: "error" }>) => {
    if (!failed) on(msg);
    failed = true;
  };
  const handle = (msg: WorkerMsg) => {
    if (msg.type === "established") clearTimeout(establish);
    if (msg.type === "stopped") stopped?.();
    else if (msg.type === "error") fail(msg);
    else on(msg);
  };
  const discard = () => {
    clearTimeout(establish);
    worker.onmessage = worker.onerror = null;
    worker.terminate();
    stopped?.();
  };
  worker.onmessage = (event: MessageEvent<WorkerMsg>) => handle(event.data);
  worker.onerror = (event) =>
    fail({
      type: "error",
      recoverable: true,
      detail: event.message || "worker error",
    });
  worker.postMessage({ type: "start", ...start });
  const establish = session
    ? setTimeout(
        () =>
          fail({
            type: "error",
            recoverable: true,
            detail: "webtransport session did not establish",
          }),
        ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS,
      )
    : undefined;
  return {
    measure: (seq) => worker.postMessage({ type: "measure", seq }),
    discard,
    // A session worker finalizes its upload and acknowledges before release.
    stop: () =>
      !session
        ? Promise.resolve(discard())
        : new Promise((resolve) => {
            const timer = setTimeout(discard, STOP_GRACE_MS);
            stopped = () => {
              stopped = null;
              clearTimeout(timer);
              discard();
              resolve();
            };
            worker.postMessage({ type: "stop" });
          }),
  };
}

export const laneWorker = (kind: "fetch" | "wt"): Worker =>
  kind === "fetch"
    ? new Worker(new URL("./workers/fetch-worker.ts", import.meta.url), {
        type: "module",
      })
    : new Worker(new URL("./workers/wt-transfer-worker.ts", import.meta.url), {
        type: "module",
      });

/** One direction's lanes: restarts, readiness and a measured-progress watchdog. */
class LaneSet {
  measuring = false;
  stalled = false;
  #lanes: (Lane | null)[] = [];
  #timers: ReturnType<typeof setTimeout>[] = [];
  #ready = new Set<number>();
  #seq = 0;
  #live = true;
  #watchdog: ReturnType<typeof setTimeout> | undefined;
  #progressAt = 0;

  constructor(
    readonly stage: ServerStage,
    readonly dir: FlowDirection,
    readonly count: number,
    readonly open: (index: number, on: (msg: WorkerMsg) => void) => Lane,
  ) {
    const stagger = laneStaggerMs(
      count,
      stage.host.config.duration.warmupMs,
      LANE_STAGGER_MS,
    );
    for (let i = 0; i < count; i++) this.#schedule(i, i * stagger);
  }

  get ready(): boolean {
    return this.#live && this.#ready.size === this.count;
  }

  /** An unstaggered lane opens at once, so its worker loads while the page's own server answers. */
  #schedule(index: number, delayMs: number): void {
    clearTimeout(this.#timers[index]);
    const open = () => {
      if (!this.#live) return;
      const lane = this.open(index, (msg) => this.#message(index, msg));
      this.#lanes[index] = lane;
      // A restarted download joins the current measurement epoch.
      if (this.measuring && this.dir === "down") lane.measure(this.#seq);
    };
    if (delayMs > 0) this.#timers[index] = setTimeout(open, delayMs);
    else open();
  }

  measure(): void {
    this.measuring = true;
    this.progress(0);
    if (this.dir === "up") return;
    this.#seq++;
    for (const lane of this.#lanes) lane?.measure(this.#seq);
  }

  setStalled(stalled: boolean, detail?: string, cause?: RecoveryCause): void {
    if (this.stalled === stalled) return;
    this.stalled = stalled;
    this.stage.stallChanged(detail, cause, this.dir);
  }

  /** Measured progress re-arms one timer; silence past the window stalls this direction alone. */
  progress(bytes: number): void {
    if (!this.measuring || !this.#live) return;
    this.#progressAt = performance.now();
    if (bytes > 0) this.setStalled(false);
    if (this.#watchdog === undefined) this.#check(DIRECTION_PROGRESS_WINDOW_MS);
  }

  #check(delayMs: number): void {
    this.#watchdog = setTimeout(() => {
      this.#watchdog = undefined;
      if (!this.measuring || !this.#live) return;
      const quietMs = performance.now() - this.#progressAt;
      if (quietMs < DIRECTION_PROGRESS_WINDOW_MS)
        this.#check(DIRECTION_PROGRESS_WINDOW_MS - quietMs);
      else this.setStalled(true, `${this.dir} direction carried no data`);
    }, delayMs);
  }

  #message(index: number, msg: WorkerMsg): void {
    if (!this.#live) return;
    const { host } = this.stage;
    if (msg.type === "progress") {
      if (msg.bytes > 0 && !this.#ready.has(index)) {
        this.#ready.add(index);
        this.stage.readinessChanged();
      }
      if (this.dir !== "down" || !this.measuring || msg.seq !== this.#seq)
        return;
      host.download(msg.bytes);
      this.progress(msg.bytes);
    } else if (msg.type === "alive") {
      if (
        this.dir === "up" &&
        msg.bytes !== undefined &&
        msg.elapsedMs !== undefined
      )
        host.uploadHint(index, msg.bytes, msg.elapsedMs);
    } else if (msg.type === "upload-progress")
      this.stage.receiver?.accept(msg.msg);
    else if (msg.type === "auth-required")
      this.stage.authenticationRequired("throughput");
    else if (msg.type === "error")
      this.#error(index, msg.recoverable, msg.detail, msg.cause);
  }

  #error(
    index: number,
    recoverable: boolean,
    detail: string,
    cause?: RecoveryCause,
  ): void {
    this.#ready.delete(index);
    // The run owns the one allowed upload-id rotation.
    if (cause === "unknown-upload-id" && this.measuring)
      return this.setStalled(true, detail, cause);
    if (!recoverable)
      return this.stage.host.fail(
        "protocol-error",
        `${this.dir} stream ${index} failed: ${detail}`,
      );
    if (this.measuring) this.setStalled(true, detail);
    this.#lanes[index]?.discard();
    this.#lanes[index] = null;
    this.#schedule(index, LANE_RESTART_BACKOFF_MS);
  }

  /** Graceful stop lets session lanes deliver terminal counters. */
  async stop(): Promise<void> {
    this.#clear();
    await Promise.all(this.#lanes.map((lane) => lane?.stop()));
    this.#live = false;
  }

  discard(): void {
    this.#live = false;
    this.#clear();
    for (const lane of this.#lanes) lane?.discard();
  }

  #clear(): void {
    clearTimeout(this.#watchdog);
    for (const timer of this.#timers) clearTimeout(timer);
  }
}

/** A disposable stage; no continuation can reach a later stage's resources. */
export class ServerStage implements StageTransport {
  readonly host: ParticipantHost;
  readonly #paths: PreparedPaths;
  readonly #activity: PhaseActivity;
  readonly #streams: Record<FlowDirection, number>;
  readonly #seed: string;
  readonly #abort = new AbortController();
  #lanes: Partial<Record<FlowDirection, LaneSet>> = {};
  receiver: UploadReceiver | null = null;
  #checkpoint: Promise<ReceiverCheckpoint | null> | null = null;
  #latency: LatencyChannel | null = null;
  #stalled = false;
  readinessChanged = () => {};

  constructor({ host, paths, activity, streams, seed }: StageOptions) {
    this.host = host;
    this.#paths = paths;
    this.#activity = activity;
    this.#streams = streams;
    this.#seed = seed;
  }

  async prepare(): Promise<void> {
    const { latency, credentials } = this.#paths;
    if (needsPings(this.#activity) && latency) {
      const cfg = this.host.config;
      const idle = this.#activity.stage === "latency";
      this.#latency = new LatencyChannel({
        host: this.host,
        target: latency.target,
        credentials,
        ready: () => this.readinessChanged(),
      });
      this.#latency.prime(idle ? cfg.pingCadence : cfg.loadedPingCadence, idle);
    } else if (this.#activity.stage === "latency")
      throw new Error("server offers no supported ping transport");
    if (this.#activity.transfer.includes("down")) this.#open("down");
    if (this.#activity.transfer.includes("up"))
      await this.#prepareUpload(this.#abort.signal);
  }

  async ready(owner: AbortSignal): Promise<void> {
    const signal = AbortSignal.any([
      owner,
      this.#abort.signal,
      AbortSignal.timeout(ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS),
    ]);
    try {
      for (;;) {
        const changed = new Promise<void>(
          (resolve) => (this.readinessChanged = resolve),
        );
        if (
          !(this.#lanes.down?.ready ?? true) ||
          !(this.#latency?.ready ?? true)
        ) {
          await abortable(changed, signal);
          continue;
        }
        if (!this.#activity.transfer.includes("up")) return;
        // Receiver evidence is checked last; a transient failure retries within this budget.
        const checkpoint = await this.checkpoint(signal).catch(() => null);
        if ((checkpoint?.nanos ?? 0) > 0) return;
        await abortableDelay(50, signal);
      }
    } catch {
      throw new Error("Primed measurement connections did not become ready", {
        cause: signal.reason,
      });
    }
  }

  measure(): void {
    for (const lanes of Object.values(this.#lanes)) lanes.measure();
    this.#latency?.measure();
  }

  async finish(): Promise<void> {
    try {
      const latency = this.#latency?.finish();
      await Promise.all(
        Object.values(this.#lanes).map((lanes) => lanes.stop()),
      );
      if (!this.#abort.signal.aborted) await this.receiver?.finish();
      await latency;
    } finally {
      this.discard();
    }
  }

  discard(incomplete = false): void {
    if (this.#abort.signal.aborted) return;
    this.#abort.abort();
    for (const lanes of Object.values(this.#lanes)) lanes.discard();
    this.receiver?.close();
    if (incomplete) this.#latency?.discard();
    else this.#latency?.teardown();
  }

  /** Old lane callbacks capture the old receiver, which closes before a replacement exists. */
  async replaceUpload(signal: AbortSignal): Promise<void> {
    this.receiver?.close();
    this.#lanes.up?.discard();
    this.receiver = null;
    delete this.#lanes.up;
    try {
      await this.#prepareUpload(signal);
    } catch (cause) {
      if (!signal.aborted && !this.#abort.signal.aborted)
        this.host.fail(
          "protocol-error",
          cause instanceof Error ? cause.message : String(cause),
        );
      return;
    }
    const lanes = this.#lanes.up as LaneSet | undefined;
    if (signal.aborted || !lanes) return;
    lanes.measure();
    lanes.setStalled(true, "awaiting replacement upload progress");
  }

  stallChanged(
    detail?: string,
    recoveryCause?: RecoveryCause,
    direction?: FlowDirection,
  ): void {
    const stalled = Object.values(this.#lanes).some((lanes) => lanes.stalled);
    if (this.#abort.signal.aborted || stalled === this.#stalled) return;
    this.#stalled = stalled;
    if (!stalled) return this.host.resume();
    const transport = this.#paths.throughput.target.transport;
    this.host.stall({
      reason: "connection-lost",
      transport,
      detail,
      recoveryCause,
      direction,
    });
  }

  authenticationRequired(role: ConnectionRole): void {
    this.discard();
    reportServerAuthentication(this.#paths.credentials, this.host, role);
    this.host.fail(
      "connection-lost",
      `Sign in again to measure ${role === "throughput" ? "throughput" : "latency"}`,
    );
  }

  async #prepareUpload(owner: AbortSignal): Promise<void> {
    const id = await this.#mint(owner);
    if (owner.aborted || this.#abort.signal.aborted) return;
    const { throughput } = this.#paths;
    const wt =
      throughput.target.transport !== "fetch-stream" ? throughput.target : null;
    const feed = wt
      ? `${wt.origin}${ROUTES.uploadProgress}?id=${encodeURIComponent(id)}`
      : `${throughput.fetch.origin}${ROUTES.uploadProgress}?id=${encodeURIComponent(id)}`;
    const receiver = new UploadReceiver(
      this,
      id,
      feed,
      this.#paths.credentials,
      !!wt,
    );
    this.receiver = receiver;
    // A session lane carries its own feed; HTTP lanes write only after the feed is open.
    if (wt) this.#open("up", id, feed);
    if (!(await receiver.opened))
      throw new Error("upload progress feed did not open");
    if (!wt && receiver === this.receiver) this.#open("up", id);
  }

  #open(dir: FlowDirection, uploadId?: string, progressUrl?: string): void {
    const { throughput, credentials } = this.#paths;
    const target = throughput.target;
    const fetchTarget = throughput.fetch;
    const streams = this.#streams[dir];
    const { headers, credentials: mode } = requestOptions(
      credentials,
      fetchTarget.origin,
      dir === "up" ? "POST" : "GET",
    );
    if (target.transport !== "fetch-stream") {
      const datagrams = target.transport === "webtransport-datagram";
      const path = dir === "down" ? ROUTES.wtDownload : ROUTES.wtUpload;
      const query =
        dir === "down"
          ? `bytes=${PER_STREAM_BYTES}&${datagrams ? "datagrams=1" : `streams=${streams}`}`
          : `id=${encodeURIComponent(uploadId!)}${datagrams ? "&datagrams=1" : ""}`;
      const start = {
        url: `${target.origin}${path}?${query}`,
        dir,
        lanes: streams,
        datagrams,
        mint: socketMint(credentials, target.origin, path, "wt"),
        progressUrl,
        headers,
        credentials: mode,
      };
      this.#lanes[dir] = new LaneSet(this, dir, 1, (_, on) =>
        openLane(laneWorker("wt"), start, true, on),
      );
    } else {
      const spec = { dir, base: fetchTarget.origin, cbSeed: this.#seed };
      this.#lanes[dir] = new LaneSet(this, dir, streams, (index, on) =>
        openLane(
          laneWorker("fetch"),
          {
            dir,
            url: laneUrl(spec, index, uploadId),
            streams,
            credentials: mode,
            headers,
          },
          false,
          on,
        ),
      );
    }
  }

  get uploadLanes(): LaneSet | undefined {
    return this.#lanes.up;
  }

  checkpoint(
    signal: AbortSignal,
    fresh = false,
  ): Promise<ReceiverCheckpoint | null> {
    signal.throwIfAborted();
    if (fresh || !this.#checkpoint) {
      const task = this.#requestCheckpoint().finally(() => {
        if (this.#checkpoint === task) this.#checkpoint = null;
      });
      this.#checkpoint = task;
    }
    return abortable(this.#checkpoint, signal);
  }

  async #requestCheckpoint(): Promise<ReceiverCheckpoint | null> {
    const receiver = this.receiver;
    if (!receiver || this.#abort.signal.aborted) return null;
    const target = this.#paths.throughput.fetch;
    const requestedAtMs = this.host.now();
    const response = await measurementFetch(
      this.#paths.credentials,
      `${target.origin}${ROUTES.uploadCheckpoint}?id=${encodeURIComponent(receiver.id)}`,
      {
        method: "POST",
        cache: "no-store",
        signal: AbortSignal.any([
          this.#abort.signal,
          AbortSignal.timeout(CHECKPOINT_TIMEOUT_MS),
        ]),
      },
    );
    if (!response.ok) return null;
    const { bytes, nanos } = (await readJSONResponse(response)) as Record<
      string,
      unknown
    >;
    if (
      !Number.isSafeInteger(bytes) ||
      !Number.isSafeInteger(nanos) ||
      (bytes as number) < 0 ||
      (nanos as number) < 0
    )
      return null;
    if (this.#abort.signal.aborted || receiver !== this.receiver) return null;
    const checkpoint = {
      id: receiver.id,
      bytes: bytes as number,
      nanos: nanos as number,
      requestedAtMs,
      receivedAtMs: this.host.now(),
    };
    receiver.observe(checkpoint);
    return checkpoint;
  }

  async #mint(owner: AbortSignal): Promise<string> {
    const target = this.#paths.throughput.fetch;
    const signal = AbortSignal.any([
      owner,
      this.#abort.signal,
      AbortSignal.timeout(ESTABLISH_BUDGET_MS),
    ]);
    try {
      const response = await measurementFetch(
        this.#paths.credentials,
        `${target.origin}${ROUTES.uploadSession}`,
        {
          method: "POST",
          cache: "no-store",
          signal,
        },
      );
      if (!response.ok)
        throw new Error(`upload session returned HTTP ${response.status}`);
      const id = parseResponseToken(
        await readJSONResponse(response),
        "uploadId",
      );
      signal.throwIfAborted();
      return id;
    } catch (cause) {
      await classifyServerAuthentication(this.#paths.credentials, signal);
      throw new Error("upload session could not be established", { cause });
    }
  }
}

type FeedEvent =
  ProgressEvent | { type: "stall"; detail: string } | { type: "auth-required" };

/** One upload id's receiver counters; replacing the id creates a new receiver. */
class UploadReceiver {
  readonly opened: Promise<boolean>;
  #open!: (ready: boolean) => void;
  #bytes = 0;
  #nanos = 0;
  #closed = false;
  #completed = false;
  #finishing: (() => void) | null = null;
  #feed: { finalize(): void; dispose(): void } | null = null;
  #poll = new AbortController();
  #observedAt = performance.now();
  #checkpointAt = -Infinity;

  /** A session lane carries its own feed and finalizes it; this side only backs that up. */
  constructor(
    readonly stage: ServerStage,
    readonly id: string,
    readonly url: string,
    readonly credentials: ServerCredentials | undefined,
    readonly session: boolean,
  ) {
    const timer = setTimeout(
      () => this.#open(false),
      ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS,
    );
    this.opened = new Promise((resolve) => {
      this.#open = (ready) => {
        clearTimeout(timer);
        resolve(ready);
      };
    });
    if (session) return;
    const { headers } = requestOptions(credentials, url, "POST");
    const { credentials: mode } = requestOptions(credentials, url);
    this.#feed = uploadFeed({
      url,
      csrf: headers,
      credentials: mode,
      onEvent: (event) => this.accept(event),
    });
    void this.#watch();
  }

  /** A quiet feed is backed by same-receiver checkpoints; a failed check proves nothing. */
  async #watch(): Promise<void> {
    while (!this.#closed) {
      await new Promise((resolve) => setTimeout(resolve, 250));
      if (this.#closed || performance.now() - this.#observedAt < 500) continue;
      await this.stage.checkpoint(this.#poll.signal).catch(() => {});
    }
  }

  observe(checkpoint: ReceiverCheckpoint): void {
    if (this.#closed || checkpoint.id !== this.id) return;
    this.#open(true);
    if (checkpoint.nanos > this.#nanos && checkpoint.bytes >= this.#bytes)
      this.#checkpointAt = performance.now();
    this.#advance(checkpoint.bytes, checkpoint.nanos, checkpoint);
  }

  accept(event: FeedEvent): void {
    if (this.#closed) return;
    const lanes = this.stage.uploadLanes;
    const measuring = !!lanes?.measuring;
    if (event.type === "open") this.#open(true);
    else if (event.type === "auth-required")
      this.stage.authenticationRequired("throughput");
    else if (event.type === "fatal") {
      this.#open(false);
      if (
        !measuring ||
        ["capacity-refusal", "owner-mismatch", "protocol-refusal"].includes(
          event.cause,
        )
      )
        this.stage.host.fail("protocol-error", event.detail);
      else lanes!.setStalled(true, event.detail, event.cause);
    } else if (event.type === "stall") {
      if (measuring && performance.now() - this.#checkpointAt >= 500)
        lanes!.setStalled(true, event.detail);
    } else {
      this.#advance(event.n, event.t);
      if (event.type === "complete") {
        this.#completed = true;
        if (this.#finishing) this.close();
      }
    }
  }

  /** Only an advancing receiver count proves delivery; warmup counters stay out of measurement. */
  #advance(
    bytes: number,
    nanos: number,
    checkpoint?: ReceiverCheckpoint,
  ): void {
    if (bytes < this.#bytes || nanos < this.#nanos) return;
    if (nanos > this.#nanos) this.#observedAt = performance.now();
    const delta = bytes - this.#bytes;
    [this.#bytes, this.#nanos] = [bytes, nanos];
    const lanes = this.stage.uploadLanes;
    if (!lanes?.measuring) return;
    this.stage.host.receiver(
      checkpoint ?? {
        id: this.id,
        bytes,
        nanos,
        receivedAtMs: this.stage.host.now(),
      },
    );
    lanes.progress(delta);
  }

  /** Session lanes have already delivered terminal records; an HTTP feed gets a bounded final grace. */
  finish(): Promise<void> {
    this.#open(false);
    if (this.session && !this.#completed && !this.#closed)
      void measurementFetch(this.credentials, this.url, {
        method: "DELETE",
        cache: "no-store",
        keepalive: true,
      }).catch(() => {});
    if (!this.#feed || this.#completed || this.#closed) {
      this.close();
      return Promise.resolve();
    }
    return new Promise((resolve) => {
      const timer = setTimeout(() => this.close(), PROGRESS_FINAL_GRACE_MS);
      this.#finishing = () => {
        clearTimeout(timer);
        resolve();
      };
      this.#feed!.finalize();
    });
  }

  close(): void {
    this.#closed = true;
    this.#poll.abort();
    this.#open(false);
    this.#feed?.dispose();
    this.#feed = null;
    this.#finishing?.();
    this.#finishing = null;
  }
}

/** The HTTP receiver feed: reconnects with backoff, never regresses its counters, and finalizes with DELETE. */
export function uploadFeed(options: {
  url: string;
  csrf: Record<string, string>;
  credentials: RequestCredentials;
  onEvent: (event: FeedEvent) => void;
}): { finalize(): void; dispose(): void } {
  const { url, csrf, credentials, onEvent } = options;
  const controller = new AbortController();
  const { signal } = controller;
  const counters = { lastN: 0, lastT: 0 };
  const redirect = redirectForCredentials(credentials);
  let finishing = false;
  let backoff = 0;
  let wake: (() => void) | undefined;

  const dispose = () => {
    controller.abort();
    wake?.();
  };
  const emit = (event: FeedEvent) => {
    if (signal.aborted) return;
    onEvent(event);
    if (
      event.type === "complete" ||
      event.type === "fatal" ||
      event.type === "auth-required"
    )
      dispose();
  };
  const expired = async () => {
    if (signal.aborted || credentials !== "include") return false;
    const required = await sessionAuthenticationRequired(
      location.origin,
      signal,
      (input, init) =>
        fetch(input, {
          ...init,
          signal: AbortSignal.any([
            signal,
            ...(init?.signal ? [init.signal] : []),
          ]),
        }),
    );
    if (required && !signal.aborted) emit({ type: "auth-required" });
    return required && !signal.aborted;
  };
  const read = async () => {
    while (!signal.aborted) {
      let detail = "progress stream closed";
      try {
        const response = await fetch(url, {
          priority: "high",
          cache: "no-store",
          headers: {
            ...(csrf.Authorization
              ? { Authorization: csrf.Authorization }
              : {}),
            accept: "application/x-ndjson",
          },
          signal,
          credentials,
          redirect,
        });
        if (!response.ok || signal.aborted)
          void response.body?.cancel().catch(() => {});
        if (signal.aborted) return;
        if (authenticationRequired(response))
          return emit({ type: "auth-required" });
        const status = response.status;
        if ((status >= 400 && status < 500) || status === 503)
          return emit({
            type: "fatal",
            detail: `progress returned HTTP ${status}`,
            cause: classifyUploadFailure(
              status,
              response.headers.get("X-Graphite-Upload-Refusal"),
            ),
          });
        if (!response.ok || !response.body)
          throw new Error(`progress returned HTTP ${status}`);
        await readProgressFeed(response.body, counters, (event) => {
          if (event.type === "open") backoff = 0;
          emit(event);
        });
      } catch (error) {
        if (signal.aborted || (await expired())) return;
        detail = String(error);
      }
      if (signal.aborted) return;
      emit({ type: "stall", detail });
      backoff = backoff ? Math.min(backoff * 2, 2000) : 100;
      await new Promise<void>((resolve) => {
        const timer = setTimeout((wake = resolve), backoff);
        if (signal.aborted) {
          clearTimeout(timer);
          resolve();
        }
      });
      wake = undefined;
    }
  };
  const finish = async () => {
    if (signal.aborted || finishing) return;
    finishing = true;
    wake?.();
    try {
      const response = await fetch(url, {
        priority: "high",
        method: "DELETE",
        cache: "no-store",
        headers: csrf,
        signal,
        credentials,
        redirect,
      });
      void response.body?.cancel().catch(() => {});
      if (signal.aborted) return;
      if (authenticationRequired(response)) emit({ type: "auth-required" });
      else if (!response.ok) dispose();
    } catch {
      if (!signal.aborted) await expired();
      dispose();
    }
  };
  void read();
  return { finalize: () => void finish(), dispose };
}
