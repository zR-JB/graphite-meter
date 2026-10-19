// One upload id's receiver: every feed observation passes one monotonic guard, and the feed's carrier finalizes the id once.
import { DecodeError } from "../../lib/decode";
import { readJSON, release } from "../../protocol/documents";
import type { FeedRecord } from "../../protocol/messages";
import { checkpoint as counters } from "../../protocol/progress";
import { ROUTES } from "../../protocol/routes";
import { SignInRequired, type Checkpoint, type Ending, type ServerAuth } from "../types";
import { Tickets, withToken } from "./dial";
import { Backoff, lost, refusal, refused, REVOKED } from "./endings";
import { FINAL_GRACE_MS, parse, readRecords } from "./records";
import { abortable, delay } from "../run/clock";

/** One checkpoint request's bound, and the final checkpoint's whole window. */
const CHECKPOINT_MS = 1_500;
const FINAL_RETRY_MS = 100;
const READY_POLL_MS = 50;
/** A feed whose receiver time stood still this long is backed by a checkpoint. */
const QUIET_MS = 250;
/** The feed sends a record at least every second: one silent for this many quiet checks is dead and reconnects. */
const SILENT_CHECKS = 6;

type End = { ending: Ending; broken: boolean };
export type FeedRoute = "session" | "http" | "websocket";
const SIGNED_OUT: Ending = {
  reason: "sign-in-required",
  retry: false,
  detail: "progress feed revoked",
};

/** Calls `silent` once the feed went unheard for every quiet check; the page clock keeps pace in a hidden tab. */
function watch(signal: AbortSignal, silent: () => void): () => void {
  let checks = 0;
  const check = (): void =>
    void delay(QUIET_MS, signal).then(() => {
      if (signal.aborted) return;
      if (++checks >= SILENT_CHECKS) silent();
      else check();
    });
  check();
  return () => (checks = 0);
}

export interface ReceiverHost {
  now(): number;
  /** An observation that passed the guard; `grew` when the receiver's bytes did. */
  observed(checkpoint: Checkpoint, grew: boolean): void;
  /** Settles the feed's ending; true when it should reconnect, after `delayMs`. */
  ended(ending: Ending, broken: boolean, delayMs: number, signal: AbortSignal): Promise<boolean>;
  signIn(): void;
}

export class Receiver {
  readonly #host: ReceiverHost;
  readonly #auth: ServerAuth;
  readonly #origin: string;
  readonly #feed: string;
  readonly #checkpoint: string;
  /** Null when a session carries the feed. */
  readonly #backoff: Backoff | null;
  readonly #tickets = new Tickets();
  readonly #closed = new AbortController();
  readonly #opened = Promise.withResolvers<void>();
  /** Aborted by a terminal record that passed the guard. */
  readonly #complete = new AbortController();
  #last: Checkpoint | null = null;
  /** Receiver time advanced since the quiet check last looked. */
  #advanced = true;
  #inFlight: Promise<Checkpoint | null> | null = null;

  constructor(
    host: ReceiverHost,
    auth: ServerAuth,
    origin: string,
    readonly id: string,
    route: FeedRoute,
  ) {
    this.#host = host;
    this.#auth = auth;
    this.#origin = origin;
    const query = `?id=${encodeURIComponent(id)}`;
    this.#feed = `${origin}${ROUTES.uploadProgress}${query}`;
    this.#checkpoint = `${origin}${ROUTES.uploadCheckpoint}${query}`;
    this.#backoff = route === "session" ? null : new Backoff();
    if (route !== "session") void this.#follow(route === "websocket");
  }

  /** The feed's DELETE, for a session that carries it. */
  get feed(): string {
    return this.#feed;
  }

  /** Refused as busy since the feed last opened. */
  get busy(): boolean {
    return !!this.#backoff?.busy;
  }

  /** Resolves once the feed is attached or the receiver observed. */
  opened(signal: AbortSignal): Promise<void> {
    return abortable(this.#opened.promise, signal);
  }

  record(record: FeedRecord): void {
    if (this.#closed.signal.aborted) return;
    if (record.type === "ready") {
      this.#backoff?.reset();
      return this.#opened.resolve();
    }
    const observed = this.#observe({
      id: this.id,
      bytes: record.bytes,
      nanos: record.nanos,
      receivedAtMs: this.#host.now(),
    });
    // A stale terminal record is no evidence that the id finalized.
    if (observed && record.type === "complete") this.#complete.abort();
  }

  /** Resolves once the receiver accepted a chunk (nanos > 0), asking for a checkpoint every 50 ms meanwhile. */
  async accepted(signal: AbortSignal): Promise<void> {
    while (!this.#last?.nanos) {
      try {
        await this.checkpoint(signal);
      } catch (error) {
        if (signal.aborted || error instanceof SignInRequired) throw error;
      }
      if (this.#last?.nanos) return;
      await delay(READY_POLL_MS, signal);
      signal.throwIfAborted();
    }
  }

  /** While measuring without a session, a quiet feed is backed by same-receiver checkpoints; a failed one proves nothing. */
  measure(): void {
    if (!this.#backoff) return;
    const check = () => {
      if (!this.#advanced)
        this.checkpoint(this.#closed.signal).catch(
          (error) => error instanceof SignInRequired && this.#host.signIn(),
        );
      this.#advanced = false;
      void delay(QUIET_MS, this.#closed.signal).then(() => this.#closed.signal.aborted || check());
    };
    check();
  }

  /** An ordinary checkpoint joins the one in flight; a final one asks afresh and retries a miss within its bound. */
  checkpoint(signal: AbortSignal, final = false): Promise<Checkpoint | null> {
    if (final) return this.#final(AbortSignal.any([signal, AbortSignal.timeout(CHECKPOINT_MS)]));
    this.#inFlight ??= this.#request().finally(() => (this.#inFlight = null));
    return abortable(this.#inFlight, signal);
  }

  /** Without a session, finalizes the id and waits a grace for its terminal record; a session's unfinalized id is backed up at once. */
  async finish(): Promise<void> {
    if (!this.#closed.signal.aborted && !this.#complete.signal.aborted) {
      if (!this.#backoff)
        // The browser bounds a keepalive request; a deadline here could end it before a slow path even connects.
        void this.#auth
          .fetch(this.#feed, { method: "DELETE", cache: "no-store", keepalive: true })
          .then(
            (response) => release(response),
            () => {},
          );
      else
        try {
          const signal = AbortSignal.any([
            this.#closed.signal,
            AbortSignal.timeout(FINAL_GRACE_MS),
          ]);
          const response = await this.#auth.fetch(this.#feed, {
            method: "DELETE",
            cache: "no-store",
            signal,
          });
          release(response);
          if (response.ok)
            await delay(
              FINAL_GRACE_MS,
              AbortSignal.any([this.#closed.signal, this.#complete.signal]),
            );
        } catch {
          // Finalization is cleanup: the measured window already closed.
        }
    }
    this.close();
  }

  close(): void {
    this.#closed.abort();
  }

  /** A regressing observation is stale: receiver bytes and time are only ever taken together. */
  #observe(checkpoint: Checkpoint): boolean {
    const { bytes = 0, nanos = 0 } = this.#last ?? {};
    if (checkpoint.bytes < bytes || checkpoint.nanos < nanos) return false;
    if (checkpoint.nanos > nanos) this.#advanced = true;
    this.#last = checkpoint;
    this.#opened.resolve();
    this.#host.observed(checkpoint, checkpoint.bytes > bytes);
    return true;
  }

  async #follow(socket: boolean): Promise<void> {
    const { signal } = this.#closed;
    for (;;) {
      const end = await (socket ? this.#listen(signal) : this.#read(signal));
      if (!end) return;
      if (end.ending.reason === "sign-in-required") return this.#host.signIn();
      const { ending, broken } = end;
      if (!(await this.#host.ended(ending, broken, this.#backoff!.after(ending), signal))) return;
    }
  }

  async #read(signal: AbortSignal): Promise<End | null> {
    const silent = new AbortController();
    const connection = AbortSignal.any([signal, silent.signal]);
    try {
      const response = await this.#auth.fetch(this.#feed, {
        cache: "no-store",
        headers: { accept: "application/x-ndjson" },
        signal: connection,
      });
      if (!response.ok || !response.body) {
        release(response);
        const ending = refused(response, `progress returned HTTP ${response.status}`);
        return { ending, broken: false };
      }
      // Only an open feed promises its lines; opening one over a loaded uplink can take seconds.
      const heard = watch(connection, () => silent.abort());
      const last = await readRecords(response.body, (record) => this.record(record), heard);
      if (last?.type === "complete") return null;
      const ending = last
        ? refusal(null, last.code, last.message || "upload progress error")
        : lost("progress stream closed");
      return { ending, broken: false };
    } catch (error) {
      if (signal.aborted) return null;
      if (error instanceof SignInRequired) return { ending: SIGNED_OUT, broken: false };
      if (silent.signal.aborted)
        return { ending: lost("progress stream went silent"), broken: false };
      return { ending: lost(String(error)), broken: true };
    } finally {
      silent.abort();
    }
  }

  async #listen(signal: AbortSignal): Promise<End | null> {
    let socket: WebSocket;
    try {
      const ticket = this.#auth.ticket(this.#origin, ROUTES.wsProgress);
      const minted = ticket ? await this.#tickets.take(ticket, signal) : null;
      if (signal.aborted) return null;
      if (minted?.type === "ended") return { ending: minted.ending, broken: false };
      if (minted?.type === "broken") return { ending: lost(minted.detail), broken: true };
      const feed = `${this.#origin.replace(/^http/, "ws")}${ROUTES.wsProgress}?id=${encodeURIComponent(this.id)}`;
      socket = new WebSocket(withToken(feed, minted?.token ?? ""));
      // However a WebSocket dial ends, it may have reached the server.
      if (minted) this.#tickets.spent(minted.token);
    } catch (error) {
      if (error instanceof SignInRequired) return { ending: SIGNED_OUT, broken: false };
      return {
        ending: { reason: "protocol-error", retry: false, detail: String(error) },
        broken: false,
      };
    }
    return new Promise((resolve) => {
      const closed = new AbortController();
      let heard = () => {};
      const done = (end: End | null) => {
        closed.abort();
        signal.removeEventListener("abort", abort);
        socket.onopen = socket.onmessage = socket.onclose = null;
        socket.close();
        resolve(end);
      };
      const abort = () => done(null);
      signal.addEventListener("abort", abort);
      // Only an open feed promises its messages; opening one over a loaded uplink can take seconds.
      socket.onopen = () =>
        (heard = watch(closed.signal, () =>
          done({ ending: lost("progress socket went silent"), broken: false }),
        ));
      socket.onmessage = ({ data }: MessageEvent<unknown>) => {
        heard();
        let record: ReturnType<typeof parse>;
        try {
          record = typeof data === "string" ? parse(data) : null;
        } catch (error) {
          return done({ ending: lost(String(error)), broken: false });
        }
        if (!record) return;
        if (record.type === "error")
          return done({
            ending: refusal(null, record.code, record.message || "upload progress error"),
            broken: false,
          });
        this.record(record);
        if (record.type === "complete") done(null);
      };
      socket.onclose = ({ code, reason }) =>
        done({
          ending:
            code === REVOKED.socket && reason === REVOKED.reason
              ? SIGNED_OUT
              : lost("progress socket closed"),
          broken: true,
        });
    });
  }

  async #final(signal: AbortSignal): Promise<Checkpoint | null> {
    for (;;) {
      signal.throwIfAborted();
      try {
        const answer = await abortable(this.#request(), signal);
        if (answer) return answer;
      } catch (error) {
        if (signal.aborted || error instanceof SignInRequired) throw error;
      }
      if (this.#closed.signal.aborted) return null;
      await delay(FINAL_RETRY_MS, signal);
    }
  }

  /** One checkpoint, or none on refusal; an observation that overtook the answer is newer, so it answers instead. */
  async #request(): Promise<Checkpoint | null> {
    if (this.#closed.signal.aborted) return null;
    const requestedAtMs = this.#host.now();
    const signal = AbortSignal.any([this.#closed.signal, AbortSignal.timeout(CHECKPOINT_MS)]);
    const response = await this.#auth.fetch(this.#checkpoint, {
      method: "POST",
      cache: "no-store",
      signal,
    });
    if (!response.ok) {
      release(response);
      return null;
    }
    let answer: { bytes: number; nanos: number };
    try {
      answer = counters(await readJSON(response));
    } catch (error) {
      if (error instanceof DecodeError || error instanceof SyntaxError) return null;
      throw error;
    }
    if (this.#closed.signal.aborted) return null;
    const checkpoint = { id: this.id, ...answer, requestedAtMs, receivedAtMs: this.#host.now() };
    return this.#observe(checkpoint) ? checkpoint : { ...this.#last!, requestedAtMs };
  }
}
