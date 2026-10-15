// One upload id's receiver (api/upload.md): its feed's observations, whether the receiver follows the HTTP progress
// feed itself or a session relays them, pass one monotonic guard; checkpoints back them up on demand. Whoever carries
// the feed finalizes the id once: the HTTP feed here, a session in its worker.
import { DecodeError } from "../../lib/decode";
import { checkpoint as counters, readJSON } from "../../protocol/documents";
import type { FeedRecord } from "../../protocol/messages";
import { ROUTES } from "../../protocol/routes";
import { SignInRequired, type Checkpoint, type Ending, type ServerAuth } from "../types";
import { Backoff, lost, refusal, refused } from "./endings";
import { FINAL_GRACE_MS, readRecords } from "./records";
import { abortable, delay } from "./wait";

/** One checkpoint request's bound, and the final checkpoint's whole window. */
const CHECKPOINT_MS = 1_500;
const FINAL_RETRY_MS = 100;
const READY_POLL_MS = 50;
/** A feed whose receiver time stood still this long is backed by a checkpoint. */
const QUIET_MS = 250;
/** The feed sends a line at least every second (a record every 100 ms once bytes arrive, a heartbeat before): an HTTP
    feed silent for this many 250 ms checks, 1.25 to 1.5 s, is dead and reconnects. */
const SILENT_CHECKS = 6;

export interface ReceiverHost {
  /** Run ms. */
  now(): number;
  /** An observation that passed the guard; `grew` when the receiver's bytes did. */
  observed(checkpoint: Checkpoint, grew: boolean): void;
  /** Settles the HTTP feed's ending; true when it should reconnect, after `delayMs`. */
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
  readonly #closed = new AbortController();
  readonly #opened = Promise.withResolvers<void>();
  /** Aborted by a terminal record that passed the guard. */
  readonly #complete = new AbortController();
  /** The latest observation that passed the guard. */
  #last: Checkpoint | null = null;
  /** Receiver time advanced since the quiet check last looked. */
  #advanced = true;
  #inFlight: Promise<Checkpoint | null> | null = null;

  constructor(
    host: ReceiverHost,
    auth: ServerAuth,
    origin: string,
    readonly id: string,
    carried: boolean,
  ) {
    this.#host = host;
    this.#auth = auth;
    const query = `?id=${encodeURIComponent(id)}`;
    this.#feed = `${origin}${ROUTES.uploadProgress}${query}`;
    this.#checkpoint = `${origin}${ROUTES.uploadCheckpoint}${query}`;
    this.#backoff = carried ? null : new Backoff();
    if (!carried) void this.#follow();
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

  /** An observation from the feed, whoever carries it. */
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

  /** While measuring over HTTP, a quiet feed is backed by same-receiver checkpoints; a failed one proves nothing. */
  measure(): void {
    if (!this.#backoff) return;
    const check = () => {
      if (!this.#advanced)
        this.checkpoint(this.#closed.signal).catch((error) => error instanceof SignInRequired && this.#host.signIn());
      this.#advanced = false;
      void delay(QUIET_MS, this.#closed.signal).then(() => this.#closed.signal.aborted || check());
    };
    check();
  }

  /** An ordinary checkpoint joins the one in flight; a final one issues fresh requests and retries a miss within
      1.5 s. Either feeds the receiver like a record. */
  checkpoint(signal: AbortSignal, final = false): Promise<Checkpoint | null> {
    if (final) return this.#final(AbortSignal.any([signal, AbortSignal.timeout(CHECKPOINT_MS)]));
    this.#inFlight ??= this.#request().finally(() => (this.#inFlight = null));
    return abortable(this.#inFlight, signal);
  }

  /** Finalizes an HTTP feed's id, then waits a grace for the terminal record. A session finalizes its own id as it
      stops; one it could not finalize (its worker died or never got its command) is backed up without waiting. */
  async finish(): Promise<void> {
    if (!this.#closed.signal.aborted && !this.#complete.signal.aborted) {
      if (!this.#backoff)
        void this.#auth.fetch(this.#feed, { method: "DELETE", cache: "no-store", keepalive: true }).then(
          (response) => response.body?.cancel().catch(() => {}),
          () => {},
        );
      else
        try {
          const signal = AbortSignal.any([this.#closed.signal, AbortSignal.timeout(FINAL_GRACE_MS)]);
          const response = await this.#auth.fetch(this.#feed, { method: "DELETE", cache: "no-store", signal });
          response.body?.cancel().catch(() => {});
          if (response.ok) await delay(FINAL_GRACE_MS, AbortSignal.any([this.#closed.signal, this.#complete.signal]));
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

  /** Follows the HTTP feed until its terminal record, reconnecting as the stage allows. */
  async #follow(): Promise<void> {
    const { signal } = this.#closed;
    for (;;) {
      let ending: Ending;
      let broken = false;
      const silent = new AbortController();
      const connection = AbortSignal.any([signal, silent.signal]);
      let checks = 0;
      const watch = (): void =>
        void delay(QUIET_MS, connection).then(() => {
          if (connection.aborted) return;
          if (++checks >= SILENT_CHECKS) silent.abort();
          else watch();
        });
      try {
        const response = await this.#auth.fetch(this.#feed, {
          cache: "no-store",
          headers: { accept: "application/x-ndjson" },
          signal: connection,
        });
        if (!response.ok || !response.body) {
          response.body?.cancel().catch(() => {});
          ending = refused(response, `progress returned HTTP ${response.status}`);
        } else {
          // Only an open feed promises its lines; opening one over a loaded uplink can take seconds.
          watch();
          const last = await readRecords(
            response.body,
            (record) => this.record(record),
            () => (checks = 0),
          );
          if (last?.type === "complete") return;
          ending = last
            ? refusal(null, last.code, last.message || "upload progress error")
            : lost("progress stream closed");
        }
      } catch (error) {
        if (signal.aborted) return;
        if (error instanceof SignInRequired) return this.#host.signIn();
        ending = lost(silent.signal.aborted ? "progress stream went silent" : String(error));
        broken = !silent.signal.aborted;
      } finally {
        silent.abort();
      }
      if (!(await this.#host.ended(ending, broken, this.#backoff!.after(ending), signal))) return;
    }
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

  /** One checkpoint; a refusal, a malformed answer or a closed receiver is none. An observation that overtook the
      answer is newer, so it was taken after the request reached the receiver: it answers instead. */
  async #request(): Promise<Checkpoint | null> {
    if (this.#closed.signal.aborted) return null;
    const requestedAtMs = this.#host.now();
    const signal = AbortSignal.any([this.#closed.signal, AbortSignal.timeout(CHECKPOINT_MS)]);
    const response = await this.#auth.fetch(this.#checkpoint, { method: "POST", cache: "no-store", signal });
    if (!response.ok) {
      response.body?.cancel().catch(() => {});
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
