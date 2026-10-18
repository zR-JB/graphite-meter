// One WebTransport session per direction (api/wire.md#webtransport-routes); it finalizes an upload when it stops.
import { release } from "../../protocol/documents";
import type { EndReport, FeedRecord, SessionCommand, SessionReport } from "../../protocol/messages";
import { ByteReports, countBytes, randomBlock } from "../lanes/bytes";
import { Tickets, withToken } from "../lanes/dial";
import { abortable } from "../run/clock";
import { CLOSE_GRACE_MS, ESTABLISH_MS, lost, refusal, sessionClosed } from "../lanes/endings";
import { FINAL_GRACE_MS, readRecords } from "../lanes/records";

type Dial = Extract<SessionCommand, { type: "download" | "upload" }>;
type Upload = Extract<SessionCommand, { type: "upload" }>;

/** A datagram loop settled within one microtask checkpoint would outrun the task carrying its own stop. */
const YIELD_MS = 4;
/** An upload's datagram loop reads the clock, for its report window and its yield, once per this many datagrams. */
const CLOCK_EVERY = 16;
/** A flood of receiver progress reaches the page at most this often. */
const RECORD_MS = 50;
const UNESTABLISHED: EndReport = {
  type: "ended",
  ending: { reason: "timeout", retry: true, detail: "webtransport session did not establish" },
};

const post = (report: SessionReport) => postMessage(report);
const ignore = () => {};
const tickets = new Tickets();
const reports = new ByteReports((bytes, epoch) => post({ type: "bytes", bytes, epoch }));
let session: Session | null = null;

self.onmessage = ({ data }: MessageEvent<SessionCommand>) => {
  if (data.type === "measure") reports.measure(data.epoch);
  else if (data.type === "flush") post(reports.flushed(data.at));
  else if (data.type === "stop")
    void (session?.stop() ?? Promise.resolve()).then(() => post({ type: "stopped" }));
  else session = new Session(data);
};

// A full task: a message, then a timer.
const turn = new MessageChannel();
const turning: (() => void)[] = [];
turn.port1.onmessage = () => setTimeout(turning.shift()!);
const taskTurn = () =>
  new Promise<void>((resolve) => {
    turning.push(resolve);
    turn.port2.postMessage(null);
  });

class Session {
  #state: "dialing" | "open" | "stopping" | "ended" = "dialing";
  #stopping: Promise<void> | null = null;
  /** Aborting closes the transport and ends the dial. */
  readonly #closed = new AbortController();
  /** The feed's terminal record, or the end of its grace. */
  readonly #completed = Promise.withResolvers<void>();
  #grace: ReturnType<typeof setTimeout> | undefined;

  constructor(readonly command: Dial) {
    if (command.type === "download") reports.measure(command.epoch);
    void this.#dial().catch(this.#broken);
  }

  /** Stops moving bytes. An upload id is finalized whatever became of the session; only a live feed can bring the
      terminal record, so only an open session waits for it before closing. */
  stop(): Promise<void> {
    return (this.#stopping ??= this.#stop());
  }

  async #stop(): Promise<void> {
    const open = this.#state === "open";
    if (open || this.#state === "dialing") this.#state = "stopping";
    reports.flush();
    if (!open) this.#closed.abort();
    if (this.command.type === "upload") await this.#finalize(this.command.finalize, open);
    this.#closed.abort();
  }

  /** The first ending is the session's; the rest are its echoes. */
  #end(report: EndReport): void {
    clearTimeout(this.#grace);
    if (this.#state !== "dialing" && this.#state !== "open") return;
    this.#state = "ended";
    reports.flush();
    post(report);
    this.#closed.abort();
  }

  /** An open session's broken stream may be the session closing, whose code then names the ending. */
  #broken = (error: unknown): void => {
    const report: EndReport = { type: "broken", detail: String(error) };
    if (this.#state !== "open") return this.#end(report);
    this.#grace ??= setTimeout(() => this.#end(report), CLOSE_GRACE_MS);
  };

  async #dial(): Promise<void> {
    const { command } = this;
    const expired = AbortSignal.timeout(ESTABLISH_MS);
    const budget = AbortSignal.any([this.#closed.signal, expired]);
    let token = "";
    if (command.ticket) {
      const minted = await tickets.take(command.ticket, budget);
      if (this.#state !== "dialing") return;
      if (minted.type !== "token") return this.#end(budget.aborted ? UNESTABLISHED : minted);
      token = minted.token;
    }
    let transport: WebTransport;
    try {
      transport = new WebTransport(withToken(command.url, token), {
        congestionControl: "throughput",
      });
    } catch (error) {
      return this.#broken(error);
    }
    this.#closed.signal.addEventListener("abort", () => {
      try {
        transport.close();
      } catch {}
    });
    transport.closed.then(
      ({ closeCode }) => this.#end(endOf(closeCode)),
      () => this.#end(endOf(undefined)),
    );
    try {
      // A black-holed handshake can leave `ready` and `closed` pending, so the budget ends the dial.
      await abortable(transport.ready, budget);
    } catch (error) {
      return this.#end(budget.aborted ? UNESTABLISHED : { type: "broken", detail: String(error) });
    } finally {
      // An answer, a refusal too, spent the ticket; a dial that lapsed may never have reached the server.
      if (!expired.aborted) tickets.spent(token);
    }
    if (this.#state !== "dialing") return;
    this.#state = "open";
    if (command.type === "upload") return this.#upload(transport, command);
    void (command.datagrams ? this.#readDatagrams(transport) : this.#acceptStreams(transport));
  }

  /** Each server-opened stream is one sized request, replaced when exhausted. */
  async #acceptStreams(transport: WebTransport): Promise<void> {
    try {
      const incoming = transport.incomingUnidirectionalStreams.getReader();
      for (let next = await incoming.read(); !next.done; next = await incoming.read())
        countBytes(next.value, reports.count).catch(this.#broken);
    } catch (error) {
      this.#broken(error);
    }
  }

  /** Every datagram is counted with its own clock reading, so a cutoff knows which side of it each one fell on;
      the reading also serves the report window and the yield. */
  async #readDatagrams(transport: WebTransport): Promise<void> {
    try {
      const reader = transport.datagrams.readable.getReader();
      let yielded = performance.now();
      for (let next = await reader.read(); !next.done; next = await reader.read()) {
        const now = performance.now();
        reports.count(next.value.byteLength, now);
        if (now - yielded < YIELD_MS) continue;
        await taskTurn();
        yielded = performance.now();
      }
    } catch (error) {
      this.#broken(error);
    }
  }

  #upload(transport: WebTransport, { datagrams, streams }: Upload): void {
    // The feed is read first, so the server reports before the first byte.
    void this.#relay(transport);
    const block = randomBlock();
    if (datagrams) void this.#sendDatagrams(transport, block);
    else for (let i = 0; i < streams; i++) void this.#writeStream(transport, block);
  }

  /** The first server-opened stream is the feed; a later one carries a refusal for a lane. */
  async #relay(transport: WebTransport): Promise<void> {
    try {
      const incoming = transport.incomingUnidirectionalStreams.getReader();
      for (let next = await incoming.read(); !next.done; next = await incoming.read())
        void this.#follow(next.value);
    } catch (error) {
      this.#broken(error);
    }
  }

  /** Records flow during the stop too, so the terminal one arrives. A refusal ends the session; a feed that ends
      before it can be recovered by a fresh session, as the server reopens the feed. */
  async #follow(stream: ReadableStream<Uint8Array>): Promise<void> {
    // Progress counts are cumulative: a flood reaches the page as its newest; every other record goes at once.
    let held: FeedRecord | null = null;
    let sentAt = Number.NEGATIVE_INFINITY;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const send = (record: FeedRecord | null) => {
      clearTimeout(timer);
      timer = undefined;
      if (held) post({ type: "record", record: held });
      if (record) post({ type: "record", record });
      if (held || record?.type === "progress") sentAt = performance.now();
      held = null;
    };
    const relay = (record: FeedRecord) => {
      const since = performance.now() - sentAt;
      if (record.type !== "progress" || since >= RECORD_MS) return send(record);
      held = record;
      timer ??= setTimeout(() => send(null), RECORD_MS - since);
    };
    try {
      let last: Awaited<ReturnType<typeof readRecords>>;
      try {
        last = await readRecords(stream, relay);
      } finally {
        if (held) send(null);
      }
      if (last?.type === "complete") return this.#completed.resolve();
      this.#end({
        type: "ended",
        ending: last
          ? refusal(null, last.code, last.message || "upload progress error")
          : lost("upload progress stream ended early"),
      });
    } catch (error) {
      this.#broken(error);
    }
  }

  async #writeStream(transport: WebTransport, block: Uint8Array<ArrayBuffer>): Promise<void> {
    try {
      const writer = (await transport.createUnidirectionalStream()).getWriter();
      while (this.#state === "open") {
        await writer.ready;
        await writer.write(block);
      }
      // A stop ends the stream with its FIN.
      await writer.close();
    } catch (error) {
      this.#broken(error);
    }
  }

  async #sendDatagrams(transport: WebTransport, block: Uint8Array<ArrayBuffer>): Promise<void> {
    const { datagrams } = transport;
    try {
      const writer = datagrams.writable.getWriter();
      let payload: Uint8Array<ArrayBuffer> | null = null;
      let yielded = performance.now();
      let unclocked = 0;
      while (this.#state === "open") {
        // The path MTU estimate can shrink, and an oversized datagram is dropped with a resolved write: the size is
        // read again after each yield.
        if (!payload) {
          const size = Math.min(block.length, datagrams.maxDatagramSize);
          if (!size)
            return this.#end({
              type: "ended",
              ending: lost("webtransport datagram size collapsed"),
            });
          payload = block.subarray(0, size);
        }
        await writer.ready;
        writer.write(payload).catch(ignore);
        if (++unclocked < CLOCK_EVERY) continue;
        unclocked = 0;
        if (performance.now() - yielded < YIELD_MS) continue;
        await taskTurn();
        yielded = performance.now();
        payload = null;
      }
    } catch (error) {
      this.#broken(error);
    }
  }

  /** One DELETE ends the upload id; the server then drains the lanes and sends its terminal record. */
  async #finalize({ url, request }: Upload["finalize"], feeding: boolean): Promise<void> {
    try {
      const signal = AbortSignal.timeout(FINAL_GRACE_MS);
      const response = await fetch(url, {
        ...request,
        method: "DELETE",
        cache: "no-store",
        signal,
      });
      release(response);
      if (!response.ok || !feeding) return;
    } catch {
      return;
    }
    const timer = setTimeout(this.#completed.resolve, FINAL_GRACE_MS);
    await this.#completed.promise;
    clearTimeout(timer);
  }
}

/** A close the server named, or a break. */
function endOf(code: number | undefined): EndReport {
  const ending = sessionClosed(code);
  return ending
    ? { type: "ended", ending }
    : { type: "broken", detail: "webtransport session closed" };
}
