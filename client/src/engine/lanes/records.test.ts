import { expect, test } from "bun:test";
import type { FeedRecord } from "../../protocol/messages";
import { readRecords } from "./records";

test("records split across reads, heartbeats and bad lines read as the feed sent them", async () => {
  const encoder = new TextEncoder();
  const chunks = [
    '{"type":"ready"}\n{"type":"progress","bytes":100,"na',
    'nos":1000}\n\n{"type":"progress","bytes":"7","nanos":9}\n{"type":"complete",',
    '"bytes":100,"nanos":1100}\n{"type":"progress","bytes":200,"nanos":1200}\n',
  ];
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      controller.close();
    },
  });
  const emitted: FeedRecord[] = [];
  const terminal = await readRecords(body, (record) => emitted.push(record));
  expect(emitted).toEqual([
    { type: "ready" },
    { type: "progress", bytes: 100, nanos: 1_000 },
    { type: "complete", bytes: 100, nanos: 1_100 },
  ]);
  expect(terminal).toEqual({ type: "complete", bytes: 100, nanos: 1_100 });
});
