import { expect, test } from "bun:test";
import { progressWindow, readBytes } from "./progressWindow";

test("download readers release their stream lock after success and failure", async () => {
  for (const type of [undefined, "bytes"] as const) {
    let consumed = 0;
    const body = new ReadableStream<Uint8Array>({
      type,
      start(
        controller:
          | ReadableStreamDefaultController<Uint8Array>
          | ReadableByteStreamController,
      ) {
        (controller as ReadableStreamDefaultController<Uint8Array>).enqueue(
          new Uint8Array(37),
        );
        controller.close();
      },
    });
    await readBytes(body, (n) => {
      consumed += n;
    });
    expect(consumed).toBe(37);
    expect(body.locked).toBe(false);
    const failure = new Error("connection lost");
    const broken = new ReadableStream<Uint8Array>({
      type,
      start(controller) {
        controller.error(failure);
      },
    });
    await expect(readBytes(broken, () => {})).rejects.toBe(failure);
    expect(broken.locked).toBe(false);
  }
});

test("batches bytes until the reporting cadence", () => {
  const progress = progressWindow(100);

  expect(progress.add(10, 149)).toBeNull();
  expect(progress.add(20, 150)).toEqual({ bytes: 30, elapsedMs: 50 });
});

test("flush returns the final partial window once", () => {
  const progress = progressWindow(100);

  expect(progress.add(17, 120)).toBeNull();
  expect(progress.flush(125)).toEqual({ bytes: 17, elapsedMs: 25 });
  expect(progress.flush(130)).toBeNull();
});

// The consumer divides bytes by elapsed time, so a window closed at the clock reading it opened at has no denominator.
test("a window with bytes but no elapsed time is not a measurement", () => {
  const progress = progressWindow(100);

  expect(progress.add(30, 150)).toEqual({ bytes: 30, elapsedMs: 50 });
  expect(progress.add(10, 150)).toBeNull();
  expect(progress.flush(150)).toBeNull();
});

test("reset discards bytes from the preceding measurement sequence", () => {
  const progress = progressWindow(100);

  expect(progress.add(17, 120)).toBeNull();
  progress.reset(200);
  expect(progress.flush(250)).toBeNull();
});
