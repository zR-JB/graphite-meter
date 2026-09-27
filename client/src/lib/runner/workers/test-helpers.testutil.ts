import { stubGlobals } from "../../test-helpers.testutil";

export function testClock() {
  let time = 0;
  let nextId = 0;
  const timers = new Map<
    number,
    { at: number; everyMs?: number; callback: () => void }
  >();
  const schedule = (callback: () => void, delayMs = 0, everyMs?: number) => {
    const id = ++nextId;
    timers.set(id, { at: time + delayMs, everyMs, callback });
    return id;
  };
  const clear = (timer: unknown): void => {
    timers.delete(timer as number);
  };
  return {
    now: () => time,
    setTimeout: (callback: () => void, delayMs?: number) =>
      schedule(callback, delayMs),
    clearTimeout: clear,
    setInterval: (callback: () => void, everyMs: number) =>
      schedule(callback, everyMs, everyMs),
    clearInterval: clear,
    /** Move time without firing timers, as a suspended realm would. */
    jump(ms: number) {
      time += ms;
    },
    advance(ms: number) {
      const end = time + ms;
      for (;;) {
        const next = [...timers.entries()]
          .filter(([, timer]) => timer.at <= end)
          .sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0];
        if (!next) break;
        const [id, timer] = next;
        time = Math.max(time, timer.at);
        if (timer.everyMs) timer.at = time + timer.everyMs;
        else timers.delete(id);
        timer.callback();
      }
      time = end;
    },
  };
}

export interface WorkerRealm<Out> {
  posted: Out[];
  send(message: unknown): void;
  restore(): void;
}

let realms = 0;

/** Evaluate a fresh copy of a worker module against stubbed globals. */
export async function bootWorker<Out>(
  modulePath: string,
  globals: Record<string, unknown> = {},
): Promise<WorkerRealm<Out>> {
  const posted: Out[] = [];
  const restore = stubGlobals({
    onmessage: null,
    postMessage: (message: Out) => void posted.push(message),
    ...globals,
  });
  await import(`${modulePath}?realm=${realms++}`);
  const handler = globalThis.onmessage as (event: MessageEvent) => void;
  return {
    posted,
    restore,
    send: (data) => handler({ data, origin: "" } as MessageEvent),
  };
}

const finished = (...chunks: Uint8Array[]) =>
  new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(chunk);
      controller.close();
    },
  });

/** A WebTransport session whose handshake, lanes and datagrams the test drives. */
export class FakeWebTransport {
  readonly ready: Promise<void>;
  readonly closed: Promise<WebTransportCloseInfo>;
  readonly accept: () => void;
  readonly refuse: (cause: unknown) => void;
  readonly end: (info?: WebTransportCloseInfo) => void;
  readonly sent: string[] = [];
  closes = 0;
  lanesOpened = 0;
  #lanes!: ReadableStreamDefaultController<ReadableStream<Uint8Array>>;
  #datagrams!: ReadableStreamDefaultController<Uint8Array>;
  readonly incomingUnidirectionalStreams = new ReadableStream<
    ReadableStream<Uint8Array>
  >({ start: (controller) => void (this.#lanes = controller) });
  datagrams: {
    readable: ReadableStream<Uint8Array>;
    writable: WritableStream<Uint8Array>;
    readonly maxDatagramSize: number;
  } = {
    maxDatagramSize: 1200,
    readable: new ReadableStream<Uint8Array>({
      start: (controller) => void (this.#datagrams = controller),
    }),
    writable: new WritableStream<Uint8Array>({
      write: (datagram) =>
        void this.sent.push(new TextDecoder().decode(datagram)),
    }),
  };

  constructor(readonly url: string) {
    const ready = Promise.withResolvers<void>();
    const closed = Promise.withResolvers<WebTransportCloseInfo>();
    this.ready = ready.promise;
    this.closed = closed.promise;
    this.ready.catch(() => {});
    this.closed.catch(() => {});
    this.accept = ready.resolve;
    this.refuse = (cause) => (ready.reject(cause), closed.reject(cause));
    this.end = (info = { closeCode: 0, reason: "" }) => closed.resolve(info);
  }

  lane(...chunks: Uint8Array[]): void {
    this.incoming(finished(...chunks));
  }

  incoming(stream: ReadableStream<Uint8Array>): void {
    this.#lanes.enqueue(stream);
  }

  endLanes(): void {
    this.#lanes.close();
  }

  datagram(data: string | Uint8Array): void {
    this.#datagrams.enqueue(
      typeof data === "string" ? new TextEncoder().encode(data) : data,
    );
  }

  endDatagrams(): void {
    this.#datagrams.close();
  }

  createUnidirectionalStream(): Promise<WritableStream<Uint8Array>> {
    this.lanesOpened++;
    return Promise.resolve(
      new WritableStream<Uint8Array>({ write: () => new Promise(() => {}) }),
    );
  }

  close(): void {
    this.closes++;
    this.end();
  }
}

export function fakeWebTransport(
  open: (session: FakeWebTransport) => void = (session) => session.accept(),
) {
  const sessions: FakeWebTransport[] = [];
  return {
    sessions,
    WebTransport: class extends FakeWebTransport {
      constructor(url: string) {
        super(url);
        sessions.push(this);
        open(this);
      }
    },
  };
}
