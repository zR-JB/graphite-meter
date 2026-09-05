import { test, expect, afterEach } from "bun:test";
import {
  readProgressFeed,
  type ProgressEvent,
  type ProgressFeedState,
} from "./progressFeed";

const realParse = JSON.parse;
const realDecode = TextDecoder.prototype.decode;
afterEach(() => {
  JSON.parse = realParse;
  TextDecoder.prototype.decode = realDecode;
});

function feedOf(...lines: string[]): ReadableStream<Uint8Array> {
  const body = new TextEncoder().encode(lines.join("\n"));
  return new ReadableStream({
    start(controller) {
      controller.enqueue(body);
      controller.close();
    },
  });
}

async function read(
  stream: ReadableStream<Uint8Array>,
  state: ProgressFeedState = { lastN: 0, lastT: 0 },
): Promise<{ events: ProgressEvent[]; end: string; state: ProgressFeedState }> {
  const events: ProgressEvent[] = [];
  const end = await readProgressFeed(stream, state, (e) => events.push(e));
  return { events, end, state };
}

// The SAME fixture the Go refusal test asserts against (go/internal/endpoint/upload_owner_test.go).
const refusalPinPath = `${import.meta.dir}/../../../../../api/uploadrefusals.txt`;

function parseRefusalPin(text: string): Record<string, string> {
  const pinned: Record<string, string> = {};
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (line === "" || line.startsWith("#")) continue;
    const parts = line.split("|");
    if (parts.length !== 3)
      throw new Error(`line ${i + 1}: want 3 fields: ${line}`);
    pinned[parts[0].trim()] = parts[1].trim();
  }
  if (Object.keys(pinned).length === 0) throw new Error("pin is empty");
  return pinned;
}

const refusals = parseRefusalPin(await Bun.file(refusalPinPath).text());

// The error record the server sends a refused lane, which is all a WebTransport lane gets: no status line, so the.
function refusalRecord(name: string, message: string): string {
  return `{"type":"error","code":${JSON.stringify(name)},"message":${JSON.stringify(message)}}`;
}

// Superseded feeds can replay stale receiver observations.
test("stale receiver observations are discarded", async () => {
  const { events } = await read(
    feedOf(
      `{"type":"ready"}`,
      `{"type":"progress","bytes":500,"nanos":1}`,
      `{"type":"progress","bytes":200,"nanos":2}`,
      `{"type":"progress","bytes":900,"nanos":3}`,
      "",
    ),
  );
  expect(events.map((e) => ("n" in e ? e.n : e.type))).toEqual([
    "open",
    500,
    900,
  ]);
});

// A session restart reattaches to the same server-side aggregate.
test("the receiver pair carries across a replacement feed", async () => {
  const state: ProgressFeedState = { lastN: 0, lastT: 0 };
  await read(
    feedOf(`{"type":"ready"}`, `{"type":"progress","bytes":800,"nanos":8}`, ""),
    state,
  );
  const { events } = await read(
    feedOf(`{"type":"ready"}`, `{"type":"progress","bytes":300,"nanos":3}`, ""),
    state,
  );
  expect(events).toEqual([{ type: "open" }]);
});

// A replacement feed replays the handshake for an upload the caller already considers open.
test("a repeated ready record opens the feed once", async () => {
  const { events } = await read(
    feedOf(
      `{"type":"ready"}`,
      `{"type":"ready"}`,
      `{"type":"progress","bytes":10,"nanos":1}`,
      "",
    ),
  );
  expect(events).toEqual([{ type: "open" }, { type: "bytes", n: 10, t: 1 }]);
});

test("blank heartbeats and truncated lines are not measurements", async () => {
  const { events, end } = await read(
    feedOf(
      `{"type":"ready"}`,
      "",
      "   ",
      `{"type":"progr`,
      `{"type":"progress","bytes":10,"nanos":7}`,
      "",
    ),
  );
  expect(events).toEqual([{ type: "open" }, { type: "bytes", n: 10, t: 7 }]);
  expect(end).toBe("eof");
});

// The parse attempt can: heartbeats arrive at the server's keep-warm cadence for the whole stage, and only the.
test("a blank heartbeat never reaches the parser", async () => {
  const parsed: string[] = [];
  JSON.parse = ((text: string) => {
    parsed.push(text);
    return realParse(text);
  }) as typeof JSON.parse;
  await read(feedOf(`{"type":"ready"}`, "", "   ", `{"type":"progr`, ""));
  expect(parsed).toEqual([`{"type":"ready"}`, `{"type":"progr`]);
});

test("a complete record ends the feed with receiver totals", async () => {
  const complete = await read(
    feedOf(`{"type":"ready"}`, `{"type":"complete","bytes":42,"nanos":9}`, ""),
  );
  expect(complete.end).toBe("complete");
  expect(complete.events.at(-1)).toEqual({ type: "complete", n: 42, t: 9 });
});

// Every refusal the server can send must reach the caller as a fatal carrying that exact text, not just the owner.
test("every pinned upload refusal surfaces as a fatal", async () => {
  for (const [name, message] of Object.entries(refusals)) {
    const { events, end } = await read(
      feedOf(refusalRecord(name, message), ""),
    );
    expect(end, name).toBe("fatal");
    expect(events, name).toEqual([
      {
        type: "fatal",
        detail: message,
        cause:
          name === "invalid"
            ? "unknown-upload-id"
            : name === "ownerMismatch"
              ? "owner-mismatch"
              : "capacity-refusal",
      },
    ]);
  }
});

// A record split across two reads must not be parsed twice or dropped.
test("a record spanning a chunk boundary is read once", async () => {
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      const enc = new TextEncoder();
      controller.enqueue(enc.encode(`{"type":"ready"}\n{"type":"prog`));
      controller.enqueue(enc.encode(`ress","bytes":77,"nanos":5}\n`));
      controller.close();
    },
  });
  const { events } = await read(stream);
  expect(events).toEqual([{ type: "open" }, { type: "bytes", n: 77, t: 5 }]);
});

// The decoder holds an incomplete multi-byte sequence back between chunks, and only a non-streaming decode releases.
test("the read that ends the feed flushes the decoder", async () => {
  const streaming: (boolean | undefined)[] = [];
  TextDecoder.prototype.decode = function (
    input?: AllowSharedBufferSource,
    options?: TextDecodeOptions,
  ) {
    streaming.push(options?.stream);
    return realDecode.call(this, input, options);
  };
  await read(feedOf(`{"type":"ready"}`, ""));
  expect(streaming).toEqual([true, false]);
});

test("oversized progress records stop reading, including fragmented records", async () => {
  for (const fragments of [
    ["x".repeat(65_537) + "\n"],
    ["x".repeat(40_000), "x".repeat(30_000)],
  ]) {
    let cancelled = false;
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        for (const fragment of fragments)
          controller.enqueue(new TextEncoder().encode(fragment));
      },
      cancel() {
        cancelled = true;
      },
    });
    await expect(read(stream)).rejects.toThrow("exceeds 64 Ki characters");
    expect(cancelled).toBe(true);
    expect(stream.locked).toBe(false);
  }
});

test("the record limit does not cap a chunk containing many valid records", async () => {
  const records = Array.from({ length: 2_000 }, (_, i) =>
    JSON.stringify({ type: "progress", bytes: i, nanos: i * 100_000_000 }),
  );
  const { events } = await read(feedOf(...records, ""));
  expect(events).toHaveLength(2_000);
  expect(events.at(-1)).toEqual({
    type: "bytes",
    n: 1_999,
    t: 199_900_000_000,
  });
});

test("terminal records cancel the remaining stream and release its reader", async () => {
  for (const record of [
    { type: "complete", bytes: 42, nanos: 9 },
    { type: "error", code: "ownerMismatch" },
  ]) {
    let cancelled = false;
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          new TextEncoder().encode(JSON.stringify(record) + "\n"),
        );
      },
      cancel() {
        cancelled = true;
      },
    });
    const { end } = await read(stream);
    expect(end).toBe(record.type === "complete" ? "complete" : "fatal");
    expect(cancelled).toBe(true);
    expect(stream.locked).toBe(false);
  }
});

test("receiver pairs reject stale bytes or timestamps without mixing observations", async () => {
  const { events } = await read(
    feedOf(
      '{"type":"progress","bytes":100,"nanos":10}',
      '{"type":"progress","bytes":90,"nanos":20}',
      '{"type":"progress","bytes":110,"nanos":9}',
      '{"type":"progress","bytes":120,"nanos":30}',
      "",
    ),
  );
  expect(events).toEqual([
    { type: "bytes", n: 100, t: 10 },
    { type: "bytes", n: 120, t: 30 },
  ]);
});

test("malformed and stale terminal records cannot complete a feed", async () => {
  const { events, end } = await read(
    feedOf(
      '{"type":"progress","bytes":100,"nanos":10}',
      '{"type":"complete","bytes":200}',
      '{"type":"complete","bytes":"200","nanos":20}',
      '{"type":"complete","bytes":200,"nanos":9}',
      "",
    ),
  );
  expect(end).toBe("eof");
  expect(events).toEqual([{ type: "bytes", n: 100, t: 10 }]);
});

test("an explicit zero receiver window is a terminal observation", async () => {
  const { events, end } = await read(
    feedOf('{"type":"complete","bytes":0,"nanos":0}', ""),
  );
  expect(end).toBe("complete");
  expect(events).toEqual([{ type: "complete", n: 0, t: 0 }]);
});
