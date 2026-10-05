import { expect, test } from "bun:test";
import {
  nextUploadBytes,
  downloadFailure,
  refusal,
  uploadPoolBytes,
} from "./fetch-worker";
import { bootWorker } from "./test-helpers.testutil";

test("only an admission refusal reconnects a download lane; its Retry-After is whole seconds", () => {
  for (const [status, reason, retry] of [
    [429, "server-busy", true],
    [503, "server-busy", true],
    [500, "protocol-error", false],
    [403, "protocol-error", false],
  ] as const)
    expect(downloadFailure(status)).toEqual({ reason, retry });
  const busy = (headers: HeadersInit) =>
    refusal(new Response(null, { status: 429, headers }), downloadFailure(429))
      .retryAfterMs;
  expect(
    (
      [{ "Retry-After": "1" }, {}, { "Retry-After": "0.5" }] as HeadersInit[]
    ).map((h) => busy(h)),
  ).toEqual([1_000, undefined, undefined]);
});

const MiB = 1024 * 1024;
const MIN_POST_BYTES = 128 * 1024;
const MAX_POST_BYTES = 10 * MiB;

test("the upload reservoir is bounded by device memory and divided across lanes", () => {
  for (const [lanes, deviceMemory, maxBytes, expected] of [
    [1, 8, undefined, 256 * MiB],
    [4, 8, undefined, 64 * MiB],
    [4, 2, undefined, 4 * MiB],
    [4, 4, undefined, 6 * MiB],
    [16, 2, undefined, 2 * MiB],
    [0, 8, undefined, 256 * MiB],
    [-2, 8, undefined, 256 * MiB],
    [1, undefined, undefined, 128 * MiB],
    [4, undefined, undefined, 32 * MiB],
    [1, undefined, 64 * MiB, 64 * MiB],
    [128, undefined, undefined, 2 * MiB],
  ] as const)
    expect(uploadPoolBytes(lanes, deviceMemory, maxBytes)).toBe(expected);
});

test("the upload size follows a 0.3 EWMA by at most one step within its bounds", () => {
  for (const [bytes, elapsedMs, ewma, maxBytes, expected] of [
    [
      MIN_POST_BYTES,
      200,
      0,
      MAX_POST_BYTES,
      { bytes: 256 * 1024, ewma: 655360 },
    ],
    [
      MAX_POST_BYTES,
      100000,
      0,
      MAX_POST_BYTES,
      { bytes: 5 * MiB, ewma: 104857.6 },
    ],
    [8 * MiB, 1, 0, MAX_POST_BYTES, { bytes: MAX_POST_BYTES }],
    [MIN_POST_BYTES, 100000, 0, 256 * 1024, { bytes: MIN_POST_BYTES }],
    [1000000, 1000, 2000000, MAX_POST_BYTES, { bytes: 850000, ewma: 1700000 }],
    [100000, 0, 50000, MAX_POST_BYTES, { bytes: 100000, ewma: 50000 }],
    [100000, -50, 50000, MAX_POST_BYTES, { bytes: 100000, ewma: 50000 }],
  ] as const)
    expect(nextUploadBytes(bytes, elapsedMs, ewma, maxBytes)).toMatchObject(
      expected,
    );
});

for (const streams of [1, 128])
  test(`the first upload with ${streams} lanes copies only one source block, not the full reservoir`, async () => {
    const { incompressibleBlock } = await import("./payload");
    const NativeBlob = Blob;
    let copiedBytes = 0;
    let reservoirBytes = 0;
    let firstBody: ArrayBuffer | undefined;
    const ended = Promise.withResolvers<void>();
    const realm = await bootWorker("./fetch-worker.ts", {
      self: globalThis,
      navigator: { deviceMemory: 8 },
      postMessage: () => ended.resolve(),
      Blob: class extends NativeBlob {
        constructor(parts: BlobPart[] = [], options?: BlobPropertyBag) {
          super(parts, options);
          for (const part of parts)
            if (ArrayBuffer.isView(part) || part instanceof ArrayBuffer)
              copiedBytes += part.byteLength;
          reservoirBytes = Math.max(reservoirBytes, this.size);
        }
      },
      fetch: async (_url: unknown, init: RequestInit) => {
        firstBody = await (init.body as Blob).arrayBuffer();
        // End this lane after observing its first real payload.
        return new Response(null, { status: 400 });
      },
    });
    try {
      realm.send({
        type: "start",
        dir: "up",
        url: "https://meter.test/upload?id=test",
        streams,
      });
      await ended.promise;
      expect(reservoirBytes).toBe(uploadPoolBytes(streams, 8));
      expect(copiedBytes).toBeLessThanOrEqual(4 * 1024 * 1024);
      expect(new Uint8Array(firstBody!)).toEqual(
        incompressibleBlock().subarray(0, MIN_POST_BYTES),
      );
    } finally {
      realm.restore();
    }
  });
