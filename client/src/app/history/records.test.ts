import { afterEach, expect, spyOn, test } from "bun:test";
import type { LatencySummary } from "../../engine/measure/latency";
import type { WireModel } from "../../engine/measure/overhead";
import type { RunResult, StagePlans } from "../../engine/run/result";
import type { Stage } from "../../engine/run/schedule";
import {
  historyRecord,
  naturalDescending,
  readHistoryRecord,
  recentCompletion,
  sortHistory,
  historyEntry,
  upgradeRecord,
  type HistoryRecord,
  type HistorySort,
} from "./records";

Object.assign(globalThis, { BUILD: { clientVersion: "v9.9.9" } });
afterEach(() => void delete (crypto as { randomUUID?: unknown }).randomUUID);

const rate = (bps: number) => ({
  reportedBytesPerSec: bps,
  peakBytesPerSec: bps,
  stabilityPct: 91,
  totalBytes: 4e8,
});
const population = (p50Ms: number) => ({ p50Ms, probeCount: 40 }) as LatencySummary;
const wire = {
  factors: [{ key: "ip", label: "IP", contributionPct: 2.7 }],
  mtuBytes: 1_500,
  totalMultiplier: 1.04,
};
const none = { latency: null, download: null, upload: null, bidirectional: null };
const identity = { id: "a", name: "Edge", url: "https://a.example" };
const measured = {
  latency: { reportedMs: 12, jitterMs: 1.5, stabilityPct: 88 },
  latencyByStage: {
    ...none,
    latency: population(12),
    download: population(30),
    upload: population(25),
  },
  addedLatency: { download: 18, upload: 13, bidirectional: null },
};
const stages = {
  latency: "complete",
  download: "complete",
  upload: "complete",
  bidirectional: "partial",
} as const;
const full: HistoryRecord = {
  schemaVersion: 6,
  id: "00000000-0000-4000-8000-000000000001",
  completedAt: 1_760_000_031_000,
  build: "v1.2.3",
  result: {
    ...measured,
    download: { ...rate(9e7), wire: wire as WireModel },
    upload: { ...rate(4e7), wire: null },
    bidirectional: { down: rate(6e7), up: rate(3e7), wire: null },
    multiServer: {
      selection: [identity],
      participants: ["a"],
      latencyFocus: "a",
      servers: [
        {
          server: identity,
          engineVersion: "1.2.3",
          throughput: { origin: identity.url, transport: "fetch-stream", protocol: "http2" },
          latencyTarget: { origin: identity.url, transport: "websocket" },
          ...measured,
          download: rate(9e7),
          upload: rate(4e7),
          bidirectional: { down: rate(6e7), up: rate(3e7) },
          totalBytes: { down: 1.2e9, up: 5e8 },
          stages: { ...stages },
        },
      ],
      intervals: [],
      omittedIntervals: 0,
      failures: [
        {
          serverId: "a",
          stage: "bidirectional",
          atMs: 3e4,
          scope: "throughput",
          reason: "timeout",
          message: "",
        },
      ],
    },
    outcome: "partial",
    stages,
    startedAt: 1_760_000_000_000,
    durationMs: 31_000,
  },
};

const latencyOnly: RunResult = {
  ...full.result,
  download: null,
  upload: null,
  bidirectional: null,
  latencyByStage: { ...none, latency: population(12) },
  addedLatency: null,
  multiServer: { ...full.result.multiServer, servers: [], failures: [] },
  outcome: "complete",
  stages: { latency: "complete", download: "not-run", upload: "not-run", bidirectional: "not-run" },
};
const off = { planned: false, durationMs: 10_000, early: false };
const plans: StagePlans = {
  latency: { ...off, planned: true },
  download: off,
  upload: off,
  bidirectional: off,
};

test("a finished run is saved apart from the shown result, and as incomplete when its evidence contradicts it", () => {
  const errors = spyOn(console, "error").mockImplementation(() => {});
  const cases: [string, RunResult, StagePlans, RunResult["outcome"], string[] | null][] = [
    ["coherent", latencyOnly, plans, "complete", null],
    [
      "claims more",
      { ...latencyOnly, outcome: "partial" },
      plans,
      "incomplete",
      ["outcome partial should be complete"],
    ],
    [
      "skipped a planned stage",
      latencyOnly,
      { ...plans, download: { ...off, planned: true } },
      "incomplete",
      ["download is not-run but planned true"],
    ],
    ["already incomplete", { ...latencyOnly, outcome: "incomplete" }, plans, "incomplete", null],
  ];
  for (const [name, result, settled, outcome, logged] of cases) {
    errors.mockClear();
    const shown = structuredClone(result);
    const before = Date.now();
    const record = historyRecord({ result: shown, plans: settled });
    expect(record, name).toMatchObject({ schemaVersion: 6, build: "v9.9.9" });
    expect(record.id, name).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(record.completedAt, name).toBeWithin(before, Date.now() + 1);
    expect(record.result, name).toEqual({ ...result, outcome });
    expect(shown, name).toEqual(result);
    shown.multiServer.selection[0]!.name = "changed later";
    expect(record.result.multiServer.selection[0]!.name, name).toBe("Edge");
    expect(errors.mock.calls, name).toEqual(
      logged ? [["Incoherent result saved as incomplete:", logged]] : [],
    );
    expect(readHistoryRecord(record), name).toEqual(record);
  }
  errors.mockRestore();
});

test("without randomUUID, on plain-HTTP LAN pages, ids are still v4 UUIDs", () => {
  Object.defineProperty(crypto, "randomUUID", { value: undefined, configurable: true });
  const random = spyOn(crypto, "getRandomValues");
  for (const [byte, id] of [
    [0x00, "00000000-0000-4000-8000-000000000000"],
    [0xff, "ffffffff-ffff-4fff-bfff-ffffffffffff"],
  ] as const) {
    random.mockImplementation(((bytes: Uint8Array) => bytes.fill(byte)) as never);
    expect(historyRecord({ result: latencyOnly, plans }).id).toBe(id);
  }
  random.mockRestore();
});

function altered(record: unknown, path: string, value: unknown): unknown {
  const copy = JSON.parse(JSON.stringify(record));
  const keys = path.split(".");
  keys.slice(0, -1).reduce((target, key) => target[key], copy)[keys.at(-1)!] = value;
  return copy;
}
const nested = (levels: number): unknown => (levels ? { next: nested(levels - 1) } : 1);

test("a stored row is readable only when every field History dereferences holds, within plain-data limits", () => {
  expect(readHistoryRecord(JSON.parse(JSON.stringify(full)))).toEqual(full);
  const server = "result.multiServer.servers.0";
  const cases: [string, unknown, boolean][] = [
    ["id", "not-a-uuid", true],
    ["completedAt", -8.64e15, true],
    ["result.latency", null, true],
    ["result.latencyByStage.download.p50Ms", null, true],
    ["result.download", undefined, true],
    ["result.download.wire", null, true],
    ["result.bidirectional", null, true],
    ["result.addedLatency", null, true],
    [`${server}.latencyTarget`, null, true],
    [`${server}.engineVersion`, undefined, true],
    // What no view reads is trusted as stored.
    ["result.startedAt", "unchecked", true],
    ["schemaVersion", undefined, false],
    ["schemaVersion", 5, false],
    ["id", 5, false],
    ["completedAt", 8.64e15 + 1, false],
    ["completedAt", "1760000031000", false],
    ["build", null, false],
    [`${server}.engineVersion`, 2, false],
    ["result", [], false],
    ["result.outcome", "aborted", false],
    ["result.stages", null, false],
    ["result.durationMs", "31000", false],
    ["result.latency.reportedMs", null, false],
    ["result.latencyByStage", null, false],
    ["result.latencyByStage.download.probeCount", undefined, false],
    ["result.latencyByStage.upload.p50Ms", "25", false],
    ["result.addedLatency.download", "18", false],
    ["result.download.reportedBytesPerSec", "fast", false],
    ["result.download.totalBytes", null, false],
    ["result.download.stabilityPct", undefined, false],
    ["result.download.wire.totalMultiplier", null, false],
    ["result.download.wire.mtuBytes", "1500", false],
    ["result.download.wire.factors", {}, false],
    ["result.download.wire.factors.0.label", 1, false],
    ["result.download.wire.factors.0.contributionPct", null, false],
    ["result.upload.reportedBytesPerSec", null, false],
    ["result.bidirectional.down", 5, false],
    ["result.bidirectional.up.totalBytes", "4e8", false],
    ["result.multiServer", null, false],
    ["result.multiServer.latencyFocus", null, false],
    ["result.multiServer.selection", {}, false],
    ["result.multiServer.selection.0.url", 5, false],
    ["result.multiServer.servers.0", {}, false],
    [`${server}.server.id`, 1, false],
    [`${server}.throughput.transport`, null, false],
    [`${server}.latencyTarget.transport`, 5, false],
    [`${server}.totalBytes.up`, null, false],
    [`${server}.stages`, [], false],
    [`${server}.latency.reportedMs`, "12", false],
    [`${server}.latencyByStage`, undefined, false],
    [`${server}.addedLatency.upload`, "13", false],
    [`${server}.download.reportedBytesPerSec`, null, false],
    [`${server}.bidirectional.up`, "x", false],
    ["result.multiServer.failures.0.serverId", null, false],
    ["result.multiServer.failures.0.stage", "warmup", false],
    ["result.multiServer.failures.0.reason", 5, false],
    // What an upgrade brings to this build's form is read only in that form.
    ["result.download.peakBytesPerSec", null, false],
    ["result.latency.stabilityPct", undefined, false],
    ["result.stages.download", "aborted", false],
    [`${server}.stages.latency`, "running", false],
    [`${server}.throughput.protocol`, "h2", false],
    // Plain data: bounded text, lists, objects and depth, finite numbers, nothing else.
    ["build", "x".repeat(2_048), true],
    ["build", "x".repeat(2_049), false],
    ["result.multiServer.failures.0.message", "x".repeat(2_049), false],
    ["extra", Array(512).fill(0), true],
    ["extra", Array(513).fill(0), false],
    ["extra", Object.fromEntries(Array.from({ length: 513 }, (_, i) => [i, 0])), false],
    ["extra", nested(8), true],
    ["extra", nested(9), false],
    ["result.startedAt", Number.POSITIVE_INFINITY, false],
    ["result.multiServer.failures.0.atMs", Number.NaN, false],
    ["extra", 1n, false],
    ["extra", Symbol("x"), false],
    ["extra", () => {}, false],
  ];
  for (const [path, value, readable] of cases)
    expect(
      readHistoryRecord(altered(full, path, value)) !== null,
      `${path} = ${String(value).slice(0, 20)}`,
    ).toBe(readable);
  for (const value of [null, "record", [full]]) expect(readHistoryRecord(value)).toBeNull();
});

const { engineVersion, ...unversioned } = full.result.multiServer.servers[0]!;
const v5 = {
  ...full,
  schemaVersion: 5,
  engine: engineVersion,
  result: { ...full.result, multiServer: { ...full.result.multiServer, servers: [unversioned] } },
};

test("an older build's row upgrades to this build's form, and the row itself stays as stored", () => {
  const server = "result.multiServer.servers.0";
  const cases: [string, unknown, unknown][] = [
    // Older builds saved no peak without a 500 ms window.
    ["result.download.peakBytesPerSec", null, 9e7],
    ["result.bidirectional.up.peakBytesPerSec", undefined, 3e7],
    [`${server}.upload.peakBytesPerSec`, null, 4e7],
    [`${server}.bidirectional.down.peakBytesPerSec`, "fast", 6e7],
    ["result.download.wire", undefined, null],
    ["result.bidirectional.wire", undefined, null],
    ["result.latency.stabilityPct", undefined, null],
    [`${server}.latency.stabilityPct`, undefined, null],
    [`${server}.throughput.protocol`, "h2", "http2"],
    [`${server}.throughput.protocol`, "h3", "http3"],
    [`${server}.throughput.protocol`, "http/1.1", "http1"],
    [`${server}.throughput.protocol`, "negotiated", "negotiated"],
    [`${server}.throughput.protocol`, "", "negotiated"],
    [`${server}.throughput.protocol`, "spdy/3", "negotiated"],
    // Views look stage statuses up: one this build does not know reads as not run.
    ["result.stages.download", "aborted", "not-run"],
    ["result.stages.upload", undefined, "not-run"],
    [`${server}.stages.latency`, "running", "not-run"],
    [`${server}.stages.bidirectional`, "toString", "not-run"],
  ];
  for (const [path, stored, read] of cases) {
    const row = altered(v5, path, stored);
    const before = structuredClone(row);
    expect(upgradeRecord(row), `${path} = ${stored}`).toEqual(
      altered(full, path, read) as HistoryRecord,
    );
    expect(row, path).toEqual(before);
  }
});

test("schema 5's engine version moves to the server it belonged to: the latency focus, else the first", () => {
  const second = { ...unversioned, server: { id: "b", name: "Oslo", url: "https://b.example" } };
  const both = (latencyFocus: string, engine = "1.2.3") => ({
    ...v5,
    engine,
    result: {
      ...v5.result,
      multiServer: { ...v5.result.multiServer, latencyFocus, servers: [unversioned, second] },
    },
  });
  const servers = (row: unknown) =>
    upgradeRecord(row)!.result.multiServer.servers.map((each) => [
      each.server.id,
      each.engineVersion,
    ]);
  expect(servers(both("b"))).toEqual([
    ["a", undefined],
    ["b", "1.2.3"],
  ]);
  expect(servers(both("gone"))).toEqual([
    ["a", "1.2.3"],
    ["b", undefined],
  ]);
  expect(servers(both("a", "unknown"))).toEqual([
    ["a", undefined],
    ["b", undefined],
  ]);
});

const lane4 = (center: number) => ({
  min: 9,
  max: 30,
  p10: 10,
  p90: 20,
  center,
  jitter: 2,
  timeoutRatio: 0,
  accountingComplete: true,
  count: 50,
  timeoutCount: 1,
  unresolvedCount: 2,
  sendFailureCount: 3,
});
const summary4 = (center: number) => ({
  accountingComplete: true,
  probeCount: 50,
  timeoutCount: 1,
  unresolvedCount: 2,
  sendFailureCount: 3,
  jitterPairs: 0,
  minMs: 9,
  maxMs: 30,
  meanMs: null,
  p10Ms: 10,
  p50Ms: center,
  p90Ms: 20,
  p95Ms: null,
  jitterMs: 2,
});
const breakdown = { factors: [], mtuBytes: 1_500, transport: "http2" };
const latency4 = { reportedMs: 12, jitterMs: 2, stabilityScore: 1, band: "high" };
const download4 = {
  reportedBytesPerSec: 1_000,
  peakBytesPerSec: 1_100,
  totalBytes: 5_000,
  stabilityPct: 3,
};
const flat = {
  schemaVersion: 4,
  id: "old",
  startedAt: 100,
  completedAt: 200,
  durationMs: 100,
  stages: {
    latency: {
      status: "complete",
      result: latency4,
      lanes: { ...none, latency: lane4(12), download: lane4(18) },
    },
    download: { status: "complete", result: download4 },
    upload: { status: "failed", result: null },
    bidirectional: { status: "not-run", down: null, up: null },
  },
  bufferbloat: { idleMs: 12, loadedMs: 18, increaseMs: 6, grade: "A", addedMs: { download: 6 } },
  totalBytes: 5_000,
  server: { name: "Home", location: "Here", engine: "e4" },
  transport: {
    throughput: { protocol: "h2", kind: "fetch-stream" },
    latency: { protocol: null, kind: "websocket" },
  },
  ipVersion: 6,
  client: { build: "v0.8.6" },
  failures: [{ stage: "upload", direction: "up", reason: "timeout" }],
  wireEstimates: {
    version: 2,
    breakdown: { download: breakdown, upload: null, bidirectional: breakdown },
    downloadBytesPerSec: 1_050,
    uploadBytesPerSec: null,
    bidirectionalBytesPerSec: 3_300,
  },
};
const statuses4 = {
  latency: "complete",
  download: "complete",
  upload: "failed",
  bidirectional: "not-run",
};
const results4 = {
  latency: { ...latency4, stabilityPct: null },
  latencyByStage: { ...none, latency: summary4(12), download: summary4(18) },
  addedLatency: { download: 6, upload: null, bidirectional: null },
  download: { ...download4, wire: { ...breakdown, totalMultiplier: 1.05 } },
  upload: null,
  bidirectional: null,
};
const home = { id: "self", url: "", name: "Home", location: "Here" };
/** `flat` as read: `changes` to its result, measured alike by its one server, with `entry` changes to that server. */
const lifted = (changes: object = {}, entry: object = {}, server: object = home) => {
  const { stages, outcome, ...evidence } = {
    ...results4,
    stages: statuses4,
    outcome: "incomplete",
    ...changes,
  };
  const measuredBy = {
    server,
    engineVersion: "e4",
    throughput: { origin: "", transport: "fetch-stream", protocol: "http2", clientIpVersion: 6 },
    latencyTarget: { origin: "", transport: "websocket" },
    ...evidence,
    totalBytes: { down: 5_000, up: 0 },
    stages,
    ...entry,
  };
  return {
    schemaVersion: 6,
    id: "old",
    completedAt: 200,
    build: "v0.8.6",
    result: {
      ...evidence,
      stages,
      outcome,
      multiServer: {
        selection: [server],
        participants: ["self"],
        latencyFocus: "self",
        servers: [measuredBy],
        intervals: [],
        omittedIntervals: 0,
        failures: [],
      },
      startedAt: 100,
      durationMs: 100,
    },
  };
};

test("a schema 4 row upgrades to schema 6 with its servers, without its grade or failures", () => {
  const bidirectional = {
    status: "partial",
    down: { ...download4, reportedBytesPerSec: 2_000 },
    up: download4,
  };
  const saved = { ...full.result.multiServer, servers: [{ ...unversioned, stages: {} }] };
  const cases: [string, unknown, unknown][] = [
    ["one server, its grade dropped and its wire rate a model", flat, lifted()],
    ["its own outcome kept", { ...flat, outcome: "partial" }, lifted({ outcome: "partial" })],
    [
      "both bidirectional lanes, with one wire model",
      {
        ...flat,
        stages: { ...flat.stages, upload: { status: "complete", result: null }, bidirectional },
      },
      lifted({
        stages: { ...statuses4, upload: "complete", bidirectional: "partial" },
        outcome: "partial",
        bidirectional: {
          down: bidirectional.down,
          up: download4,
          wire: { ...breakdown, totalMultiplier: 1.1 },
        },
      }),
    ],
    [
      "one bidirectional lane, without a wire model",
      { ...flat, stages: { ...flat.stages, bidirectional: { ...bidirectional, up: null } } },
      lifted({
        stages: { ...statuses4, bidirectional: "partial" },
        bidirectional: { down: bidirectional.down, up: null, wire: null },
      }),
    ],
    [
      "nothing optional",
      {
        ...flat,
        bufferbloat: null,
        wireEstimates: null,
        ipVersion: null,
        server: { name: "Home", location: null, engine: "e4" },
        transport: {
          throughput: { protocol: null, kind: null },
          latency: { protocol: null, kind: null },
        },
      },
      lifted(
        { addedLatency: null, download: { ...download4, wire: null } },
        { throughput: { origin: "", transport: "", protocol: "negotiated" }, latencyTarget: null },
        { id: "self", url: "", name: "Home" },
      ),
    ],
    [
      "servers it saved itself, with the record's statuses",
      { ...flat, multiServer: saved },
      {
        ...lifted(),
        result: {
          ...lifted().result,
          multiServer: {
            ...saved,
            servers: [{ ...unversioned, engineVersion: "e4", stages: statuses4 }],
          },
        },
      },
    ],
  ];
  for (const [name, row, expected] of cases) {
    expect(readHistoryRecord(row), name).toBeNull();
    expect(upgradeRecord(row) as unknown, name).toEqual(expected);
  }
  // A row no build wrote is left as it is, never a throw that would end the whole upgrade.
  const broken: [string, unknown][] = [
    ["stages.latency", null],
    ["stages.latency.lanes", null],
    ["stages.latency.lanes.download", 5],
    ["stages.download", null],
    ["stages.upload", null],
    ["stages.bidirectional", null],
    ["server", null],
    ["client", null],
    ["client.build", 1],
    ["transport.throughput", null],
    ["transport.latency", null],
    ["wireEstimates.breakdown", null],
    ["multiServer", { servers: {} }],
  ];
  for (const [path, value] of broken)
    expect(upgradeRecord(altered(flat, path, value)), path).toBeNull();
});

test("a row no release could read stays unreadable rather than upgraded into made-up stages or rates", () => {
  const server = "result.multiServer.servers.0";
  const rows = [
    ...["x", 7, [], null].map((stages) => altered(full, "result.stages", stages)),
    ...["h2", 7, []].map((bidirectional) => altered(full, "result.bidirectional", bidirectional)),
    ...["x", 7, []].map((stages) => altered(full, `${server}.stages`, stages)),
    ...["h2", 7].map((throughput) => altered(full, `${server}.throughput`, throughput)),
    ...["x", 7, []].map((stages) => altered(flat, "stages", stages)),
    ...["x", 7].map((transport) => altered(flat, "transport", transport)),
    altered(flat, "stages.download", 7),
    altered(flat, "transport.latency", "websocket"),
    altered(flat, "wireEstimates.breakdown", "x"),
    altered(flat, "stages.latency.lanes", []),
  ];
  for (const row of rows) expect(upgradeRecord(row), JSON.stringify(row).slice(0, 200)).toBeNull();
});

test("History sorts missing values last, equal ones newest first, each field in its natural direction", () => {
  const record = (id: string, completedAt: number, result: Partial<RunResult>): HistoryRecord => ({
    ...full,
    id,
    completedAt,
    result: { ...latencyOnly, ...none, latencyByStage: none, ...result },
  });
  const loaded = (medians: Partial<Record<Stage, number>>) => ({
    ...none,
    ...Object.fromEntries(
      Object.entries(medians).map(([stage, p50Ms]) => [stage, population(p50Ms)]),
    ),
  });
  const transfer = (bps: number) => ({ ...rate(bps), wire: null });
  const idle = (reportedMs: number) => ({ reportedMs, jitterMs: null, stabilityPct: null });
  const records = [
    record("a", 1, {
      download: transfer(10),
      upload: transfer(40),
      bidirectional: { down: rate(5), up: rate(5), wire: null },
      latency: idle(20),
      latencyByStage: loaded({ upload: 80 }),
    }),
    record("b", 2, {
      download: transfer(30),
      upload: transfer(20),
      bidirectional: { down: rate(20), up: rate(10), wire: null },
      latency: idle(10),
      // The highest loaded median, with or without idle latency.
      latencyByStage: loaded({ download: 20, bidirectional: 5 }),
    }),
    // A large surviving lane must not outrank a complete bidirectional result.
    record("c", 3, {
      bidirectional: { down: rate(1_000), up: null, wire: null },
      latency: idle(10),
    }),
    // Ties go newest first, then by id: ids here run against completion times.
    record("d", 5, { download: transfer(10) }),
    record("e", 4, { download: transfer(10) }),
    record("f", 5, { download: transfer(10) }),
  ].map(historyEntry);
  const cases: [HistorySort, boolean, string][] = [
    ["date", true, "fdecba"],
    ["date", false, "abcefd"],
    ["download", true, "bfdeac"],
    ["download", false, "fdeabc"],
    ["upload", true, "abfdec"],
    ["upload", false, "bafdec"],
    ["bidirectional", true, "bafdec"],
    ["idle", false, "cbafde"],
    ["idle", true, "acbfde"],
    ["loaded", false, "bafdec"],
    ["loaded", true, "abfdec"],
  ];
  for (const [sort, descending, expected] of cases) {
    const order = sortHistory(records, sort, descending).map(({ id }) => id);
    expect(order.join(""), `${sort} ${descending}`).toBe(expected);
  }
  expect(records.map(({ keys }) => keys.bidirectional)).toEqual([10, 30, null, null, null, null]);
  const natural = (["date", "download", "upload", "bidirectional", "idle", "loaded"] as const).map(
    naturalDescending,
  );
  expect(natural).toEqual([true, true, true, true, false, false]);
});

test("a recent completion reads as now, then in minutes for an hour", () => {
  const cases: [number, string | null][] = [
    [-5_000, "now"],
    [0, "now"],
    [59_999, "now"],
    [60_000, "1 min ago"],
    [3_599_999, "59 min ago"],
    [3_600_000, null],
  ];
  for (const [elapsed, expected] of cases)
    expect(recentCompletion(1_000_000, 1_000_000 + elapsed)).toBe(expected);
});
