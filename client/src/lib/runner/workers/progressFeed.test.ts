import { test, expect } from "bun:test";
import {
  readProgressFeed,
  type ProgressEvent,
  type ProgressFeedState,
} from "./progressFeed";

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

// The server counter is authoritative, so a record that goes backwards is a
// replaced feed catching up rather than bytes being un-sent. Reporting the dip
// would show the upload rate collapsing mid-stage.
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

// A session restart re-attaches to the same server-side aggregate, so the clamp
// is the caller's and outlives one feed.
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

test("a terminal record ends the feed and names why", async () => {
  const complete = await read(
    feedOf(`{"type":"ready"}`, `{"type":"complete","bytes":42,"nanos":9}`, ""),
  );
  expect(complete.end).toBe("complete");
  expect(complete.events.at(-1)).toEqual({ type: "complete", n: 42, t: 9 });

  const refused = await read(
    feedOf(
      `{"type":"error","message":"upload id belongs to another client"}`,
      "",
    ),
  );
  expect(refused.end).toBe("fatal");
  expect(refused.events).toEqual([
    { type: "fatal", detail: "upload id belongs to another client" },
  ]);
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
