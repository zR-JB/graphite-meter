import { test, expect } from "bun:test";
import {
  decodeUploadProgress,
  readProgressFeed,
  type ProgressEvent,
  type ProgressFeedState,
} from "./progressFeed";
import type { FailureReason, LaneFailure } from "../contract";
import { readPin } from "../../test-helpers.testutil";

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

// The same pin the Go refusal test asserts (go/internal/endpoint/upload_owner_test.go).
const refusals = Object.fromEntries(
  (await readPin("uploadrefusals.txt")).map(([name, message]) => [
    name,
    message,
  ]),
);

// A refused WebTransport lane gets only this error record: no status line, so the message is the whole signal.
function refusalRecord(name: string, message: string): string {
  return `{"type":"error","code":${JSON.stringify(name)},"message":${JSON.stringify(message)}}`;
}

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

const reasons = Object.fromEntries(
  (await readPin("uploadrefusalreasons.txt")) as [string, FailureReason][],
);
const DISPOSITION: Record<string, Omit<LaneFailure, "reason">> = {
  invalid: { retry: false, rotate: true },
  globalFull: { retry: true },
  clientFull: { retry: true },
  ownerMismatch: { retry: false },
  idle: { retry: true },
  revoked: { retry: false },
};

// Every refusal the server can send must reach the caller as a fatal carrying that exact text, not just the owner.
test("every pinned upload refusal surfaces as a fatal", async () => {
  for (const [name, message] of Object.entries(refusals)) {
    const { events, end } = await read(
      feedOf(refusalRecord(name, message), ""),
    );
    expect(end, name).toBe("fatal");
    expect(reasons[name], name).toBeDefined();
    expect(events, name).toEqual([
      {
        type: "fatal",
        detail: message,
        reason: reasons[name],
        ...DISPOSITION[name],
      },
    ]);
  }
  expect(Object.keys(reasons).sort()).toEqual(Object.keys(refusals).sort());
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

test("a multi-byte character split across chunks is decoded once", async () => {
  const bytes = new TextEncoder().encode(
    `{"type":"error","code":"invalid","message":"café"}\n`,
  );
  const split = bytes.indexOf(0xc3) + 1;
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(bytes.subarray(0, split));
      controller.enqueue(bytes.subarray(split));
      controller.close();
    },
  });
  const { events } = await read(stream);
  expect(events).toMatchObject([{ type: "fatal", detail: "café" }]);
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

// Superseded feeds can replay stale receiver observations.
test("receiver pairs reject stale bytes or timestamps without mixing observations", async () => {
  const { events } = await read(
    feedOf(
      '{"type":"ready"}',
      '{"type":"progress","bytes":100,"nanos":10}',
      '{"type":"progress","bytes":90,"nanos":20}',
      '{"type":"progress","bytes":110,"nanos":9}',
      '{"type":"progress","bytes":120,"nanos":30}',
      "",
    ),
  );
  expect(events).toEqual([
    { type: "open" },
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

const fixtures: { name: string; record: unknown; valid: boolean }[] =
  await Bun.file(
    new URL(
      "../../../../../api/upload-progress.testvectors.json",
      import.meta.url,
    ),
  ).json();

for (const { name, record, valid } of fixtures) {
  test(`upload progress conformance: ${name}`, () => {
    const decoded = decodeUploadProgress(record);
    expect(decoded !== null).toBe(valid);
    if (decoded !== null) {
      expect(decodeUploadProgress(JSON.parse(JSON.stringify(decoded)))).toEqual(
        decoded,
      );
    }
  });
}
