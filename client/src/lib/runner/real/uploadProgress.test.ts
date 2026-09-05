import { test, expect, afterEach } from "bun:test";
import type { CoreHost } from "../core";
import type { FetchThroughputTarget } from "../../api/endpoints";
import {
  UploadProgressChannel,
  type UploadProgressLane,
} from "./uploadProgress";

const realFetch = globalThis.fetch;
const channels: UploadProgressChannel[] = [];
afterEach(() => {
  channels.splice(0).forEach((channel) => channel.discard());
  globalThis.fetch = realFetch;
});
const target: FetchThroughputTarget = {
  id: "http://meter.test:7246",
  origin: "http://meter.test:7246",
  transport: "fetch-stream",
  protocol: "http1",
  tls: false,
  routes: {
    probe: "/probe",
    download: "/download",
    upload: "/upload",
    uploadSession: "/upload/session",
    uploadProgress: "/upload/progress",
  },
};
function channelUnderTest(
  laneState: Partial<UploadProgressLane> = {},
  sampleProvesStageLiveness = true,
  recoveryStartedAt?: number,
): {
  channel: UploadProgressChannel;
  failures: string[];
  curve: number[];
  durations: number[];
  liveness: boolean[];
  progress: number[];
  presentations: number[];
  stalls: { detail?: string; cause?: string }[];
  recoveryGaps: number[];
  recoveryBytes: number[];
} {
  const failures: string[] = [];
  /** Byte delta of every frame the channel fed into the live curve. */
  const curve: number[] = [];
  const durations: number[] = [];
  const liveness: boolean[] = [];
  const progress: number[] = [];
  const presentations: number[] = [];
  const stalls: { detail?: string; cause?: string }[] = [];
  const recoveryGaps: number[] = [];
  const recoveryBytes: number[] = [];
  const lane: UploadProgressLane = {
    stage: "upload",
    measuring: false,
    noteMeasuredProgress: (bytes) => progress.push(bytes),
    setStalled: (_stalled, detail, cause) => stalls.push({ detail, cause }),
    ...laneState,
  };
  const host = {
    failStage(_stage: string, _reason: string, message: string) {
      failures.push(message);
    },
    fail(_reason: string, message: string) {
      failures.push(message);
    },
    ingestThroughput(
      _dir: string,
      bytesDelta: number,
      duration: number,
      _authoritative: boolean,
      provesLiveness = true,
    ) {
      curve.push(bytesDelta);
      durations.push(duration);
      liveness.push(provesLiveness);
    },
    recordRecoveryGap(_dir: string, seconds: number) {
      recoveryGaps.push(seconds);
    },
    recordRecoveryBytes(_dir: string, bytes: number) {
      recoveryBytes.push(bytes);
    },
    presentationRate() {
      return 750;
    },
  } as unknown as CoreHost;
  const channel = new UploadProgressChannel({
    host,
    target,
    lane,
    recoveryStartedAt,
    sampleProvesStageLiveness: () => sampleProvesStageLiveness,
    discardTransfer: () => {},
    authoritativePresentation: (bytesPerSec) => presentations.push(bytesPerSec),
  });
  channels.push(channel);
  return {
    channel,
    failures,
    curve,
    durations,
    liveness,
    progress,
    presentations,
    stalls,
    recoveryGaps,
    recoveryBytes,
  };
}
const bytes = (channel: UploadProgressChannel, n: number, t: number): void =>
  channel.accept({ type: "bytes", n, t: t * 1_000_000_000 });

test("a discarded upload cannot feed the replacement meter", async () => {
  const old = channelUnderTest({ measuring: true });
  const oldReady = old.channel.attachExternal(() => {});
  old.channel.discard();
  const replacement = channelUnderTest({ measuring: true });
  const ready = replacement.channel.attachExternal(() => {});
  replacement.channel.accept({ type: "open" });
  bytes(old.channel, 9999, 1);
  bytes(old.channel, 19999, 2);
  bytes(replacement.channel, 100, 1);
  bytes(replacement.channel, 250, 2);
  expect(old.curve).toEqual([]);
  expect(replacement.curve).toEqual([150]);
  expect(await oldReady).toBe(false);
  expect(old.failures).toEqual([]);
  expect(await ready).toBe(true);
});
test("accept: a refusal ends a pending external attach", async () => {
  const { channel, failures } = channelUnderTest({ measuring: true });
  const attached = channel.attachExternal(() => {});

  channel.accept({
    type: "fatal",
    detail: "session closed",
    cause: "transient-connection",
  });
  // Racing an already-settled sentinel reports an attach left pending as a value rather than as a whole-test timeout.
  const outcome = await Promise.race([attached, Promise.resolve("pending")]);
  expect(outcome).toBe(false);
  expect(failures).toEqual([]);
});
test("an explicit invalid upload id starts runner-owned recovery", () => {
  const { channel, failures, stalls } = channelUnderTest({ measuring: true });

  channel.accept({
    type: "fatal",
    detail: "unknown upload id",
    cause: "unknown-upload-id",
  });

  expect(failures).toEqual([]);
  expect(stalls).toEqual([
    { detail: "unknown upload id", cause: "unknown-upload-id" },
  ]);
});
test("capacity and ownership refusals cannot trigger upload-id recovery", () => {
  for (const cause of ["capacity-refusal", "owner-mismatch"] as const) {
    const { channel, failures, stalls } = channelUnderTest({ measuring: true });
    channel.accept({ type: "fatal", detail: cause, cause });
    expect(failures).toEqual([cause]);
    expect(stalls).toEqual([]);
  }
});
test("finish finalizes a dropped session feed, but not a completed one", async () => {
  const dropped = channelUnderTest({ measuring: true });
  let droppedFinalizes = 0;
  void dropped.channel.attachExternal(() => droppedFinalizes++);
  await dropped.channel.finish();
  expect(droppedFinalizes).toBe(1);

  const { channel } = channelUnderTest({ measuring: true });
  let finalizes = 0;
  const attached = channel.attachExternal(() => finalizes++);
  channel.accept({ type: "open" });
  expect(await attached).toBe(true);

  channel.accept({ type: "complete", n: 4096, t: 1_000_000_000 });
  await channel.finish();
  expect(finalizes).toBe(0);
});
test("a server count that arrives behind the last one does not move the curve", () => {
  const { channel, curve, durations } = channelUnderTest({ measuring: true });

  bytes(channel, 1000, 1);
  bytes(channel, 2000, 2);
  bytes(channel, 500, 3);
  bytes(channel, 2600, 4);
  expect(curve).toEqual([1000, 600]);
  expect(durations).toEqual([1, 2]);
});
test("upload recovery bytes stay accounted without resuming a stalled sibling", () => {
  const { channel, curve, liveness, progress } = channelUnderTest(
    { measuring: true },
    false,
  );

  bytes(channel, 100, 1);
  bytes(channel, 250, 2);

  expect(curve).toEqual([150]);
  expect(liveness).toEqual([false]);
  expect(progress).toEqual([150]);
});
test("only an advancing server checkpoint refreshes the visual bridge baseline", () => {
  const { channel, presentations } = channelUnderTest({ measuring: true });

  bytes(channel, 100, 1);
  bytes(channel, 100, 2);
  bytes(channel, 250, 3);

  expect(presentations).toEqual([750]);
});
test("the first advancing replacement checkpoint closes a rotation gap", () => {
  const { channel, curve, progress, recoveryGaps, recoveryBytes } =
    channelUnderTest({ measuring: true }, true, performance.now());
  bytes(channel, 100, 1);
  expect(recoveryGaps).toHaveLength(1);
  expect(recoveryBytes).toEqual([100]);
  expect(progress).toEqual([100]);
  expect(curve).toEqual([]);

  bytes(channel, 250, 2);
  bytes(channel, 400, 3);

  expect(recoveryGaps).toHaveLength(1);
  expect(recoveryGaps[0]).toBeGreaterThanOrEqual(0);
  expect(recoveryBytes).toEqual([100]);
  expect(curve).toEqual([150, 150]);
  expect(progress).toEqual([100, 150, 150]);
});
function fetchFeed() {
  const requests: { url: string; init: RequestInit }[] = [];
  let writer!: ReadableStreamDefaultController<Uint8Array>;
  let body!: ReadableStream<Uint8Array>;
  globalThis.fetch = (async (
    input: RequestInfo | URL,
    init: RequestInit = {},
  ) => {
    requests.push({ url: String(input), init });
    if (init.method === "DELETE") return new Response(null, { status: 204 });
    body = new ReadableStream<Uint8Array>({
      start(controller) {
        writer = controller;
        init.signal?.addEventListener(
          "abort",
          () => controller.error(init.signal!.reason),
          { once: true },
        );
      },
    });
    return new Response(body);
  }) as typeof fetch;
  return {
    requests,
    write: (record: object) =>
      writer.enqueue(new TextEncoder().encode(JSON.stringify(record) + "\n")),
    get locked() {
      return body.locked;
    },
  };
}
async function until(predicate: () => boolean) {
  for (let i = 0; i < 100 && !predicate(); i++) await Bun.sleep(5);
  expect(predicate()).toBe(true);
}
test("HTTP readiness waits for parsed ready and finish drains final receiver counters", async () => {
  const feed = fetchFeed();
  const { channel, curve, failures } = channelUnderTest({ measuring: true });
  let ready = false;
  const primed = channel.prime("gmu_one").then((value) => {
    ready = value;
    return value;
  });
  await Bun.sleep(0);
  expect(ready).toBe(false);
  expect(feed.requests[0].url).toBe(
    "http://meter.test:7246/upload/progress?id=gmu_one",
  );
  feed.write({ type: "ready" });
  expect(await primed).toBe(true);
  feed.write({ type: "progress", bytes: 100, nanos: 1e9 });
  await Bun.sleep(0);
  let finished = false;
  const finishing = channel.finish().then(() => {
    finished = true;
  });
  expect(feed.requests.map(({ init }) => init.method ?? "GET")).toEqual([
    "GET",
    "DELETE",
  ]);
  await Bun.sleep(0);
  expect(finished).toBe(false);
  feed.write({ type: "complete", bytes: 250, nanos: 2e9 });
  await finishing;
  expect(curve).toEqual([150]);
  expect(feed.requests[0].init.signal?.aborted).toBe(true);
  expect(failures).toEqual([]);
});
test("discard settles pending HTTP readiness and final grace", async () => {
  const feed = fetchFeed();
  const { channel, failures } = channelUnderTest();
  const primed = channel.prime("gmu_one");
  await Bun.sleep(0);
  const finishing = channel.finish();
  expect(await primed).toBe(false);
  channel.discard();
  await finishing;
  expect(feed.requests.every(({ init }) => init.signal?.aborted)).toBe(true);
  await until(() => !feed.locked);
  expect(failures).toEqual([]);
});
test("discard alone settles HTTP readiness and suppresses a late ready response", async () => {
  let resolve!: (response: Response) => void;
  globalThis.fetch = (() =>
    new Promise<Response>((done) => {
      resolve = done;
    })) as unknown as typeof fetch;
  const { channel, curve } = channelUnderTest({ measuring: true });
  const primed = channel.prime("gmu_old");
  channel.discard();
  expect(await primed).toBe(false);
  resolve(
    new Response(
      '{"type":"ready"}\n{"type":"complete","bytes":9999,"nanos":1}\n',
    ),
  );
  await Bun.sleep(0);
  expect(curve).toEqual([]);
});
test("only advancing receiver bytes establish progress after a stall", () => {
  const { channel, stalls, progress } = channelUnderTest({ measuring: true });
  bytes(channel, 100, 1);
  channel.accept({ type: "stall", detail: "connection closed" });
  channel.accept({ type: "open" });
  bytes(channel, 100, 2);
  expect(progress).toEqual([]);
  expect(stalls).toEqual([{ detail: "connection closed", cause: undefined }]);
  bytes(channel, 200, 3);
  expect(progress).toEqual([100]);
});
test("missing terminal record expires grace without inventing final counters", async () => {
  const feed = fetchFeed();
  const { channel, curve, failures } = channelUnderTest({ measuring: true });
  const primed = channel.prime("gmu_one");
  feed.write({ type: "ready" });
  expect(await primed).toBe(true);
  feed.write({ type: "progress", bytes: 100, nanos: 1e9 });
  feed.write({ type: "progress", bytes: 250, nanos: 2e9 });
  await until(() => curve.length === 1);
  await channel.finish();
  expect(curve).toEqual([150]);
  expect(feed.requests[0].init.signal?.aborted).toBe(true);
  expect(failures).toEqual([]);
});
