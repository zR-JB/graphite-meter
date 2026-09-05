import { stubGlobals } from "../../test-helpers.test";
import { afterEach, expect, test } from "bun:test";
import { startUploadFeed } from "./uploadFeed";
type Event = Parameters<Parameters<typeof startUploadFeed>[0]["onEvent"]>[0];
const originalFetch = globalThis.fetch;
const owners: ReturnType<typeof startUploadFeed>[] = [];
afterEach(() => {
  owners.splice(0).forEach((owner) => owner.dispose());
  globalThis.fetch = originalFetch;
});
function start(credentials: RequestCredentials = "same-origin") {
  const events: Event[] = [];
  const owner = startUploadFeed({
    url: "https://meter.test/upload-progress?id=one",
    csrf: { "X-CSRF-Token": "token" },
    credentials,
    onEvent: (event) => events.push(event),
  });
  owners.push(owner);
  return { ...owner, events };
}
function stream(signal?: AbortSignal | null) {
  let writer!: ReadableStreamDefaultController<Uint8Array>;
  let cancelled = false;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      writer = controller;
      signal?.addEventListener("abort", () => controller.error(signal.reason), {
        once: true,
      });
    },
    cancel() {
      cancelled = true;
    },
  });
  return {
    body,
    write: (text: string) => writer.enqueue(new TextEncoder().encode(text)),
    close: () => writer.close(),
    get cancelled() {
      return cancelled;
    },
  };
}
async function until(predicate: () => boolean) {
  for (let i = 0; i < 100 && !predicate(); i++) await Bun.sleep(5);
  expect(predicate()).toBe(true);
}
function mock(
  request: (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>,
) {
  globalThis.fetch = request as typeof fetch;
}

test("fragmented NDJSON preserves receiver bytes/ns and releases terminal reader", async () => {
  const source = stream();
  mock(async () => new Response(source.body));
  const owner = start();
  source.write('{"type":"rea');
  source.write('dy"}\n{"type":"progress","bytes":77,"nanos":500000001}\n');
  source.write('{"type":"complete","bytes":91,"nanos":900000003}\n');
  await until(() => source.cancelled);
  expect(owner.events).toEqual([
    { type: "open" },
    { type: "bytes", n: 77, t: 500000001 },
    { type: "complete", n: 91, t: 900000003 },
  ]);
  expect(source.body.locked).toBe(false);
});

test("same-id EOF reconnect keeps monotonic counters and finalize wakes delay", async () => {
  let gets = 0,
    deletes = 0;
  const first = stream(),
    second = stream(),
    signals: AbortSignal[] = [];
  mock(async (_input, init) => {
    signals.push(init!.signal!);
    if (init?.method === "DELETE") {
      deletes++;
      return new Response(null, { status: 204 });
    }
    return new Response(++gets === 1 ? first.body : second.body);
  });
  const owner = start();
  first.write('{"type":"ready"}\n{"type":"progress","bytes":800,"nanos":4}\n');
  first.close();
  await until(() => owner.events.some((event) => event.type === "stall"));
  owner.finalize();
  owner.finalize();
  await until(() => gets === 2);
  second.write(
    '{"type":"ready"}\n{"type":"progress","bytes":300,"nanos":5}\n{"type":"complete","bytes":900,"nanos":6}\n',
  );
  await until(() => owner.events.at(-1)?.type === "complete");
  expect(owner.events.filter((event) => "n" in event)).toEqual([
    { type: "bytes", n: 800, t: 4 },
    { type: "complete", n: 900, t: 6 },
  ]);
  expect(deletes).toBe(1);
  expect(signals.every((signal) => signal === signals[0])).toBe(true);
  expect(signals[0].aborted).toBe(true);
});

test("authenticated DELETE keeps GET alive for terminal receiver record", async () => {
  let source!: ReturnType<typeof stream>, deletion: RequestInit | undefined;
  mock(async (_input, init) => {
    if (init?.method === "DELETE") {
      deletion = init;
      return new Response(null, { status: 204 });
    }
    expect(init?.redirect).toBe("error");
    expect(init?.headers).toEqual({ accept: "application/x-ndjson" });
    source = stream(init?.signal);
    return new Response(source.body);
  });
  const owner = start("include");
  source.write('{"type":"ready"}\n');
  await until(() => owner.events.length === 1);
  owner.finalize();
  await until(() => !!deletion);
  expect(deletion?.credentials).toBe("include");
  expect(deletion?.headers).toEqual({ "X-CSRF-Token": "token" });
  expect(deletion?.redirect).toBe("error");
  expect(deletion?.signal?.aborted).toBe(false);
  source.write('{"type":"complete","bytes":100,"nanos":9}\n');
  await until(() => owner.events.at(-1)?.type === "complete");
  expect(deletion?.signal?.aborted).toBe(true);
});

test("authentication and terminal refusal classify without reconnect", async () => {
  for (const [status, headers, expected] of [
    [403, { "Graphite-Meter-Auth": "required" }, { type: "auth-required" }],
    [
      409,
      { "X-Graphite-Upload-Refusal": "ownerMismatch" },
      {
        type: "fatal",
        detail: "progress returned HTTP 409",
        cause: "owner-mismatch",
      },
    ],
    [
      503,
      {},
      {
        type: "fatal",
        detail: "progress returned HTTP 503",
        cause: "capacity-refusal",
      },
    ],
  ] as const) {
    let calls = 0;
    mock(async () => {
      calls++;
      return new Response(null, { status, headers });
    });
    const owner = start();
    await until(() => owner.events.length > 0);
    expect(owner.events).toEqual([expected]);
    expect(calls).toBe(1);
  }
});

test("failed DELETE stops while preserving counters without completion", async () => {
  let source!: ReturnType<typeof stream>, signal!: AbortSignal;
  mock(async (_input, init) => {
    signal = init!.signal!;
    if (init?.method === "DELETE") return new Response(null, { status: 500 });
    source = stream(signal);
    return new Response(source.body);
  });
  const owner = start();
  source.write('{"type":"progress","bytes":42,"nanos":7}\n');
  await until(() => owner.events.length === 1);
  owner.finalize();
  await until(() => signal.aborted);
  expect(owner.events).toEqual([{ type: "bytes", n: 42, t: 7 }]);
});

test("dispose aborts blocked GET/DELETE and ignores late responses", async () => {
  const pending: {
    init: RequestInit;
    resolve: (response: Response) => void;
  }[] = [];
  mock(
    (_input, init) =>
      new Promise((resolve) => pending.push({ init: init!, resolve })),
  );
  const owner = start("include");
  owner.finalize();
  expect(pending.length).toBe(2);
  owner.dispose();
  expect(pending.every(({ init }) => init.signal?.aborted)).toBe(true);
  pending.forEach(({ resolve }) =>
    resolve(
      new Response(null, {
        status: 403,
        headers: { "Graphite-Meter-Auth": "required" },
      }),
    ),
  );
  await Bun.sleep(10);
  expect(owner.events).toEqual([]);
  expect(pending.length).toBe(2);
});

test("dispose cancels reconnect timer and blocked stream reader", async () => {
  let calls = 0;
  mock(async () => {
    calls++;
    throw new Error("network down");
  });
  const owner = start();
  await until(() => owner.events.length === 1);
  owner.dispose();
  await Bun.sleep(120);
  expect(calls).toBe(1);
  let source!: ReturnType<typeof stream>;
  mock(async (_input, init) => {
    source = stream(init?.signal);
    return new Response(source.body);
  });
  const blocked = start();
  await Bun.sleep(0);
  expect(source.body.locked).toBe(true);
  blocked.dispose();
  await until(() => !source.body.locked);
  expect(blocked.events).toEqual([]);
});

test("included-credential error checks session and disposal aborts that check", async () => {
  const restore = stubGlobals({ location: { origin: "https://ui.test" } });
  try {
    let checkSignal: AbortSignal | undefined;
    let resolveCheck!: (response: Response) => void;
    mock(async (input, init) => {
      if (String(input) === "https://ui.test/auth/session") {
        expect(init?.credentials).toBe("include");
        expect(init?.redirect).toBe("error");
        checkSignal = init!.signal!;
        return new Promise((resolve) => {
          resolveCheck = resolve;
        });
      }
      throw new TypeError("redirect refused");
    });
    const owner = start("include");
    await until(() => !!checkSignal);
    owner.dispose();
    expect(checkSignal?.aborted).toBe(true);
    resolveCheck(
      new Response(null, {
        status: 403,
        headers: { "Graphite-Meter-Auth": "required" },
      }),
    );
    await Bun.sleep(5);
    expect(owner.events).toEqual([]);
    const expired = start("include");
    await Bun.sleep(5);
    resolveCheck(
      new Response(null, {
        status: 403,
        headers: { "Graphite-Meter-Auth": "required" },
      }),
    );
    await until(() => expired.events.length > 0);
    expect(expired.events).toEqual([{ type: "auth-required" }]);
  } finally {
    restore();
  }
});
