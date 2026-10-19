// One upload id's receiver: every feed observation passes one monotonic guard, and the feed's carrier finalizes the id once.
import { DecodeError } from "../../lib/decode";
import { readJSON, release } from "../../protocol/documents";
import type { FeedRecord } from "../../protocol/messages";
import { checkpoint as counters } from "../../protocol/progress";
import { ROUTES } from "../../protocol/routes";
import { SignInRequired, type Checkpoint, type Ending, type ServerAuth } from "../types";
import { Tickets, withToken } from "./dial";
import { Backoff, lost, refusal, REVOKED } from "./endings";
import { FINAL_GRACE_MS, parse } from "./records";
import { abortable, delay } from "../run/clock";

/** One checkpoint request's bound, and the final checkpoint's whole window. */
const CHECKPOINT_MS = 1_500;
const FINAL_RETRY_MS = 100;
const READY_POLL_MS = 50;
/** A feed whose receiver time stood still this long is backed by a checkpoint. */
const QUIET_MS = 250;
/** The feed sends a message at least every second: a socket silent for this many quiet checks is dead and reconnects. */
const SILENT_CHECKS = 6;

type End = { ending: Ending; broken: boolean };
const SIGNED_OUT: Ending = {
  reason: "sign-in-required",
  retry: false,
  detail: "progress socket revoked",
};

export interface ReceiverHost {
  now(): number;
  /** An observation that passed the guard; `grew` when the receiver's bytes did. */
  observed(checkpoint: Checkpoint, grew: boolean): void;
  /** Settles the feed socket's ending; true when it should reconnect, after `delayMs`. */
  ended(ending: Ending, broken: boolean, delayMs: number, signal: AbortSignal): Promise<boolean>;
  signIn(): void;
}

export class Receiver {
  readonly #host: ReceiverHost;
  readonly #auth: ServerAuth;
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
    socket: string | null,
  ) {
    this.#host = host;
    this.#auth = auth;
    const query = `?id=${encodeURIComponent(id)}`;
    this.#feed = `${origin}${ROUTES.uploadProgress}${query}`;
    this.#checkpoint = `${origin}${ROUTES.uploadCheckpoint}${query}`;
    this.#backoff = socket === null ? null : new Backoff();
    if (socket !== null) void this.#follow(socket);
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

  /** While measuring, a quiet feed socket is backed by same-receiver checkpoints; a failed one proves nothing. */
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

  /** Finalizes a socket feed's id and waits a grace for its terminal record; a session's unfinalized id is backed up at once. */
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

  async #follow(origin: string): Promise<void> {
    const { signal } = this.#closed;
    for (;;) {
      const end = await this.#listen(origin, signal);
      if (!end) return;
      if (end.ending.reason === "sign-in-required") return this.#host.signIn();
      const { ending, broken } = end;
      if (!(await this.#host.ended(ending, broken, this.#backoff!.after(ending), signal))) return;
    }
  }

  async #listen(origin: string, signal: AbortSignal): Promise<End | null> {
    let socket: WebSocket;
    try {
      const ticket = this.#auth.ticket(origin, ROUTES.wsProgress);
      const minted = ticket ? await this.#tickets.take(ticket, signal) : null;
      if (signal.aborted) return null;
      if (minted?.type === "ended") return { ending: minted.ending, broken: false };
      if (minted?.type === "broken") return { ending: lost(minted.detail), broken: true };
      const feed = `${origin.replace(/^http/, "ws")}${ROUTES.wsProgress}?id=${encodeURIComponent(this.id)}`;
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
      let open = false;
      let checks = 0;
      const done = (end: End | null) => {
        clearInterval(watch);
        signal.removeEventListener("abort", abort);
        socket.onopen = socket.onmessage = socket.onclose = null;
        socket.close();
        resolve(end);
      };
      const abort = () => done(null);
      signal.addEventListener("abort", abort);
      // Only an open feed promises its messages; opening one over a loaded uplink can take seconds.
      const watch = setInterval(() => {
        if (open && ++checks >= SILENT_CHECKS)
          done({ ending: lost("progress socket went silent"), broken: false });
      }, QUIET_MS);
      socket.onopen = () => (open = true);
      socket.onmessage = ({ data }: MessageEvent<unknown>) => {
        checks = 0;
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
