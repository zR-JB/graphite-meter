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
  state: ProgressFeedState = { lastN: 0 },
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

// A row named here but absent from the pin is a renamed or dropped refusal, not an empty string to feed the parser.
function refusalMessage(name: string): string {
  const message = refusals[name];
  if (message === undefined) throw new Error(`${name} is not pinned`);
  return message;
}

// The error record the server sends a refused lane, which is all a WebTransport lane gets: no status line, so the.
function refusalRecord(name: string, message: string): string {
  return `{"type":"error","code":${JSON.stringify(name)},"message":${JSON.stringify(message)}}`;
}

// The server counter is authoritative, so a record that goes backwards is a replaced feed catching up rather than.
test("a byte count never goes backwards", async () => {
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
    500,
    900,
  ]);
});

// A session restart re-attaches to the same server-side aggregate, so the clamp is the caller's and outlives one feed.
test("the clamp carries across a replacement feed", async () => {
  const state: ProgressFeedState = { lastN: 0 };
  await read(
    feedOf(`{"type":"ready"}`, `{"type":"progress","bytes":800}`, ""),
    state,
  );
  const { events } = await read(
    feedOf(`{"type":"ready"}`, `{"type":"progress","bytes":300}`, ""),
    state,
  );
  expect(events).toEqual([{ type: "open" }, { type: "bytes", n: 800, t: 0 }]);
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

test("a terminal record ends the feed and names why", async () => {
  const complete = await read(
    feedOf(`{"type":"ready"}`, `{"type":"complete","bytes":42,"nanos":9}`, ""),
  );
  expect(complete.end).toBe("complete");
  expect(complete.events.at(-1)).toEqual({ type: "complete", n: 42, t: 9 });

  const ownerMismatch = refusalMessage("ownerMismatch");
  const refused = await read(
    feedOf(refusalRecord("ownerMismatch", ownerMismatch), ""),
  );
  expect(refused.end).toBe("fatal");
  expect(refused.events).toEqual([
    { type: "fatal", detail: ownerMismatch, cause: "owner-mismatch" },
  ]);
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
