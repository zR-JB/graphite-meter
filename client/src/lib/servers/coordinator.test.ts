import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import { stubGlobals } from "../test-helpers.testutil";
import {
  TEST_BUILD_TOKENS,
  testPreparedPaths,
} from "../runner/test-helpers.testutil";
import { DEFAULT_CONFIG } from "../state/defaults";
import type { CoreHost } from "../runner/core";
import type {
  PhaseActivity,
  ReceiverCheckpoint,
  RunResult,
  RunnerConfig,
  RunnerEvent,
} from "../runner/contract";
import type { ParticipantTransport } from "./coordinator";
import { buildHistoryRecord, isHistoryRecord } from "../history/types";
import { ServerAuthenticationRequired } from "./credentials";

let restore: () => void;
beforeEach(() => {
  restore = stubGlobals(TEST_BUILD_TOKENS);
  jest.useFakeTimers();
});
afterEach(() => {
  jest.useRealTimers();
  restore();
});

async function settle(): Promise<void> {
  for (let i = 0; i < 30; i++) await Promise.resolve();
}
/** Advance the fake clock in small steps so promise continuations interleave with timers. */
async function advance(ms: number): Promise<void> {
  for (let elapsed = 0; elapsed < ms; elapsed += 5) {
    jest.advanceTimersByTime(5);
    await settle();
  }
}

interface Peer {
  id: string;
  /** Download bytes per millisecond. */
  rate?: number;
  latency?: boolean;
  begin?(activity: PhaseActivity): void | Promise<void>;
  measure?(host: CoreHost, activity: PhaseActivity): void;
  end?(
    host: CoreHost,
    activity: PhaseActivity,
    flush: boolean,
  ): void | Promise<void>;
  checkpoint?(measuring: boolean): Promise<ReceiverCheckpoint | null>;
}

const receiver = (id: string, perMs = 3) => {
  const at = Math.round(performance.now());
  return {
    id,
    bytes: at * perMs,
    nanos: at * 1e6,
    requestedAtMs: at,
    receivedAtMs: at,
  };
};

async function coordinated(
  peers: Peer[],
  stages: Partial<RunnerConfig["stages"]>,
  duration: Partial<RunnerConfig["duration"]>,
  options: { latencySource?: string; adaptive?: boolean } = {},
) {
  const { ServerCoordinator } = await import("./coordinator");
  const calls: string[] = [];
  const events: RunnerEvent[] = [];
  const servers = peers.map((peer) => {
    const url = `https://${peer.id}.example`;
    const paths = testPreparedPaths();
    paths.throughput.target.origin = paths.throughput.fetch.origin = url;
    if (peer.latency === false) paths.latency = null;
    else paths.latency!.target.origin = url;
    return { server: { id: peer.id, url, name: peer.id }, paths };
  });
  let index = 0;
  const coordinator = new ServerCoordinator(
    servers,
    options.latencySource ?? peers[0].id,
    (): ParticipantTransport => {
      const peer = peers[index++];
      let host: CoreHost;
      let last = 0;
      let measuring = false;
      return {
        attach: (value) => (host = value),
        onRunStart() {},
        onStageBegin(activity) {
          measuring = false;
          calls.push(`begin:${peer.id}:${activity.stage}`);
          return peer.begin?.(activity);
        },
        onStageMeasure(activity) {
          measuring = true;
          last = performance.now();
          calls.push(`measure:${peer.id}`);
          peer.measure?.(host, activity);
        },
        onStageEnd(activity, flush = true) {
          measuring = false;
          calls.push(`end:${peer.id}`);
          return peer.end?.(host, activity, flush);
        },
        onAbort() {},
        onComplete() {},
        checkpoint: () =>
          peer.checkpoint?.(measuring) ?? Promise.resolve(receiver(peer.id)),
        flushDownload(now) {
          if (!measuring) return;
          host.ingestThroughput(
            "down",
            (now - last) * (peer.rate ?? 1),
            (now - last) / 1000,
          );
          last = now;
        },
      };
    },
  );
  coordinator.on((event) => events.push(event));
  const config: RunnerConfig = {
    ...structuredClone(DEFAULT_CONFIG),
    stages: {
      latency: false,
      download: false,
      upload: false,
      bidirectional: false,
      ...stages,
    },
    skipLoadedLatencyWhenStageOff: true,
    duration: {
      warmupMs: 0,
      latencyMs: 0,
      downloadMs: 0,
      uploadMs: 0,
      bidirectionalMs: 0,
      ...duration,
    },
    adaptive: { ...DEFAULT_CONFIG.adaptive, enabled: !!options.adaptive },
  };
  const terminal = () =>
    events.find((event) => event.type === "complete" || event.type === "error");
  return {
    coordinator,
    calls,
    events,
    config,
    start: () => coordinator.start(config, 0),
    /** Drive the clock until the run ends, returning its result. */
    async result(limitMs = 20_000): Promise<RunResult> {
      for (let t = 0; t < limitMs && !terminal(); t += 50) await advance(50);
      const end = terminal();
      coordinator.dispose();
      if (end?.type === "complete") return end.result;
      throw end?.type === "error" ? end.error : new Error("run did not end");
    },
  };
}
const two = (overrides: Partial<Peer>[] = []): Peer[] => [
  { id: "a", rate: 1, ...overrides[0] },
  { id: "b", rate: 3, ...overrides[1] },
];

test("one coordinated stage reports the combined path, and v4 evidence survives serialization", async () => {
  const run = await coordinated(
    two(),
    { download: true },
    { downloadMs: 1400 },
  );
  run.start();
  const result = await run.result();
  expect(run.calls.filter((call) => call.startsWith("measure:"))).toEqual([
    "measure:a",
    "measure:b",
  ]);
  expect(result.download?.reportedBytesPerSec).toBeCloseTo(4000, 0);
  expect(result.outcome).toBe("complete");
  const saved = buildHistoryRecord(result, { paths: null, clientBuild: "t" });
  expect(isHistoryRecord(JSON.parse(JSON.stringify(saved)))).toBe(true);
});

test("terminal bytes after the coordinated boundary cannot change measured totals or rates", async () => {
  const run = await coordinated(
    two().map((peer) => ({
      ...peer,
      end: (host, _activity, flush) => {
        if (flush) host.ingestThroughput("down", 1_000_000, 0, false, false);
      },
    })),
    { download: true },
    { downloadMs: 1400 },
  );
  run.start();
  const result = await run.result();
  expect(result.download?.reportedBytesPerSec).toBeCloseTo(4000, 0);
  expect(result.download?.totalBytes).toBeLessThan(10_000);
  for (const server of result.multiServer!.servers)
    expect(server.totalBytes.down).toBeLessThan(10_000);
});

test("a late dropout leaves the headline unavailable while retaining earlier measurements", async () => {
  const run = await coordinated(
    two([
      {
        measure: (host) =>
          setTimeout(
            () =>
              host.failStage("download", "connection-lost", "fixture", "down"),
            900,
          ),
      },
    ]),
    { download: true },
    { downloadMs: 1400 },
  );
  run.start();
  const result = await run.result();
  expect(result.download).toBeNull();
  expect(result.outcome).toBe("partial");
  expect(result.multiServer?.participants).toEqual(["b"]);
  expect(result.multiServer?.intervals[0].full?.downBytesPerSec).toBeCloseTo(
    4000,
    0,
  );
  expect(result.multiServer?.failures[0].serverId).toBe("a");
});

test("all participants failing emits one incomplete completion, never an aborted phase", async () => {
  const drop: Partial<Peer> = {
    measure: (host) =>
      setTimeout(
        () => host.failStage("download", "connection-lost", "fixture", "down"),
        400,
      ),
  };
  const run = await coordinated(
    two([drop, drop]),
    { download: true, upload: true },
    { downloadMs: 1400, uploadMs: 1400 },
  );
  run.start();
  const result = await run.result();
  expect(result.outcome).toBe("incomplete");
  expect(result.download).toBeNull();
  expect(result.multiServer?.participants).toEqual([]);
  expect(result.multiServer?.failures).toHaveLength(2);
  const phases = run.events.flatMap((event) =>
    event.type === "phase" ? [event.transition.to] : [],
  );
  expect(phases).not.toContain("aborted");
  expect(phases).not.toContain("upload");
  expect(run.events.filter((event) => event.type === "complete")).toHaveLength(
    1,
  );
});

test("a latency-only failure preserves both throughput participants", async () => {
  const run = await coordinated(
    two([
      {
        measure: (host) =>
          setTimeout(() => host.ingestLatencyAccountingIncomplete(), 200),
      },
    ]),
    { download: true },
    { downloadMs: 1400 },
  );
  run.coordinator.on(() => {});
  run.config.skipLoadedLatencyWhenStageOff = false;
  run.start();
  const result = await run.result();
  expect(result.multiServer?.participants).toEqual(["a", "b"]);
  expect(result.download?.reportedBytesPerSec).toBeCloseTo(4000, 0);
  expect(result.multiServer?.failures[0].scope).toBe("latency");
});

test("a throughput dropout reports discarded loaded probes before reducing the participant", async () => {
  const probe = (host: CoreHost) =>
    host.ingestLatency({
      rttMs: 10,
      lost: false,
      observedAtMs: performance.now(),
    });
  const run = await coordinated(
    two([
      {
        measure: (host) => {
          probe(host);
          setTimeout(
            () =>
              host.failStage("download", "connection-lost", "fixture", "down"),
            200,
          );
        },
        end: (host, _activity, flush) => {
          if (!flush) host.ingestLatencyAccountingIncomplete();
        },
      },
      { measure: probe },
    ]),
    { download: true },
    { downloadMs: 1400 },
  );
  run.config.skipLoadedLatencyWhenStageOff = false;
  run.start();
  const result = await run.result();
  const [failed, healthy] = result.multiServer!.servers;
  expect(failed.latencyByStage.download).toMatchObject({
    accountingComplete: false,
    probeCount: 1,
  });
  expect(healthy.latencyByStage.download).toMatchObject({
    accountingComplete: true,
    probeCount: 1,
  });
  expect(result.download?.reportedBytesPerSec).toBeCloseTo(3000, 0);
});

test("initial preparation failure requires resolving the selection", async () => {
  const run = await coordinated(
    two([
      {
        begin: () => {
          throw new Error("fixture");
        },
      },
    ]),
    { download: true },
    { downloadMs: 1400 },
  );
  run.start();
  await expect(run.result()).rejects.toMatchObject({
    reason: "protocol-error",
  });
});

test("later preparation failure removes only its server and retains the completed stage", async () => {
  const run = await coordinated(
    two([
      {
        begin: (activity) => {
          if (activity.stage === "upload") throw new Error("fixture");
        },
      },
    ]),
    { download: true, upload: true },
    { downloadMs: 1400, uploadMs: 1400 },
  );
  run.start();
  const result = await run.result();
  expect(result.download?.reportedBytesPerSec).toBeCloseTo(4000, 0);
  expect(result.upload?.reportedBytesPerSec).toBeCloseTo(3000, 0);
  expect(result.multiServer?.participants).toEqual(["b"]);
  expect(result.multiServer?.failures).toMatchObject([
    { serverId: "a", stage: "upload", reason: "preparation-failed" },
  ]);
});

test("the headline latency source is fixed before the run and is not pooled", async () => {
  const probes = (rttMs: number) => (host: CoreHost) => {
    for (let i = 0; i < 4; i++)
      host.ingestLatency({
        rttMs,
        lost: false,
        observedAtMs: performance.now(),
      });
  };
  const run = await coordinated(
    [
      { id: "a", latency: false },
      { id: "b", measure: probes(2) },
      { id: "c", measure: probes(40) },
    ],
    { latency: true, download: true },
    { latencyMs: 50, downloadMs: 1000 },
    { latencySource: "c" },
  );
  run.start();
  const result = await run.result();
  expect(run.calls.filter((call) => call.startsWith("begin:"))).toEqual([
    "begin:b:latency",
    "begin:c:latency",
    "begin:a:download",
    "begin:b:download",
    "begin:c:download",
  ]);
  expect(result.latency?.reportedMs).toBe(40);
  expect(result.multiServer?.latencyFocus).toBe("c");
  expect(result.multiServer?.servers[0].latencyByStage.latency).toBeNull();
  expect(result.multiServer?.servers[1].latency?.reportedMs).toBe(2);
});

test("adaptive stage completion retains each result before entering the next stage", async () => {
  const run = await coordinated(
    two(),
    { download: true, upload: true },
    { downloadMs: 6000, uploadMs: 6000 },
    { adaptive: true },
  );
  run.start();
  const result = await run.result();
  expect(result.download?.reportedBytesPerSec).toBeGreaterThan(0);
  expect(result.upload?.reportedBytesPerSec).toBeGreaterThan(0);
  const stageResult = run.events.findIndex(
    (event) => event.type === "stageResult" && event.stage === "download",
  );
  const upload = run.events.findIndex(
    (event) => event.type === "phase" && event.transition.to === "upload",
  );
  expect(stageResult).toBeGreaterThan(-1);
  expect(stageResult).toBeLessThan(upload);
});

test("a conflicting live stream plan is rejected without changing the running schedule", async () => {
  const run = await coordinated(
    [{ id: "self", rate: 3, latency: false }],
    { download: true },
    { downloadMs: 1200 },
  );
  run.config.transferStreams = { mode: "forced", count: 5 };
  run.start();
  await advance(100);
  expect(() =>
    run.coordinator.reconfigure({
      stages: { ...run.config.stages, upload: true },
      duration: run.config.duration,
      adaptive: run.config.adaptive,
    }),
  ).toThrow("Forced streams");
  const result = await run.result();
  expect(result.download?.reportedBytesPerSec).toBeCloseTo(3000, 0);
  expect(result.upload).toBeNull();
});

test("one missed receiver checkpoint keeps the interval, its headline and presentation evidence", async () => {
  let checkpoints = 0;
  const run = await coordinated(
    two([
      {
        // Miss exactly one measured boundary, then the final one.
        checkpoint: async (measuring) =>
          measuring && [3, 99].includes(++checkpoints) ? null : receiver("a"),
      },
    ]),
    { upload: true },
    { uploadMs: 2000 },
  );
  run.start();
  await advance(1900);
  checkpoints = 98;
  const result = await run.result();
  expect(result.upload?.reportedBytesPerSec).toBeCloseTo(6000, 0);
  expect(result.multiServer?.intervals).toHaveLength(1);
  expect(result.multiServer?.failures).toEqual([]);
  expect(
    run.events.some(
      (event) => event.type === "aggregateEvidence" && !event.available,
    ),
  ).toBe(false);
});

test("repeated missing upload checkpoints drop only the unobservable peer", async () => {
  const run = await coordinated(
    two([
      { checkpoint: async (measuring) => (measuring ? null : receiver("a")) },
    ]),
    { upload: true },
    { uploadMs: 2200 },
  );
  run.start();
  const result = await run.result();
  expect(result.outcome).toBe("partial");
  const failures = run.events.filter((event) => event.type === "serverFailure");
  expect(failures).toHaveLength(1);
  expect(failures[0]).toMatchObject({
    failure: { serverId: "a", reason: "receiver-checkpoint-failed" },
    participants: ["b"],
  });
  expect(result.upload!.reportedBytesPerSec).toBeCloseTo(3000, 0);
});

test("an expired grant during a checkpoint asks for sign-in and removes only that server", async () => {
  const run = await coordinated(
    two([
      {
        checkpoint: async (measuring) => {
          if (measuring && performance.now() > 500)
            throw new ServerAuthenticationRequired({
              id: "a",
              name: "a",
              url: "https://a.example",
            });
          return receiver("a");
        },
      },
    ]),
    { upload: true },
    { uploadMs: 2000 },
  );
  run.start();
  const result = await run.result();
  expect(result.multiServer?.failures).toMatchObject([
    { serverId: "a", reason: "sign-in-required", message: "Sign in to a" },
  ]);
  expect(result.multiServer?.participants).toEqual(["b"]);
});

test("each participant ends its stage as soon as its own final evidence arrives", async () => {
  let measuredAt = Infinity;
  const run = await coordinated(
    two([
      {
        measure: () => (measuredAt = performance.now()),
        // Regular boundaries end at 1000 ms; only the final one at 1100 ms is slow.
        checkpoint: async () => {
          if (performance.now() - measuredAt > 1050)
            await new Promise((resolve) => setTimeout(resolve, 1000));
          return receiver("a");
        },
      },
    ]),
    { upload: true },
    { uploadMs: 1100 },
  );
  run.start();
  await advance(1400);
  // b stopped while a's final checkpoint was still outstanding.
  expect(run.calls.filter((call) => call.startsWith("end:"))).toEqual([
    "end:b",
  ]);
  const result = await run.result();
  expect(result.upload?.reportedBytesPerSec).toBeCloseTo(6000, 0);
});

test("an aborted stage end delivers no late events", async () => {
  let release!: () => void;
  const run = await coordinated(
    two().map((peer) => ({
      ...peer,
      end: () => new Promise<void>((resolve) => (release = resolve)),
    })),
    { download: true, upload: true },
    { downloadMs: 1000, uploadMs: 1000 },
  );
  run.start();
  await advance(1100);
  expect(run.calls).toContain("end:a");
  run.coordinator.abort();
  const count = run.events.length;
  expect(run.events.at(-1)).toMatchObject({
    type: "phase",
    transition: { to: "aborted" },
  });
  release();
  await advance(500);
  expect(run.events).toHaveLength(count);
  run.coordinator.dispose();
});
