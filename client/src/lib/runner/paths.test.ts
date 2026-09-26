import { expect, test } from "bun:test";
import {
  CONNECTION_FRESH_MS,
  connectionDraftRoleKey,
  describeTransferStreams,
  normalizeStreamCount,
  planServerStreams,
  preparedPaths,
  roleNeedsValidation,
  summarizeRoleValidation,
  uploadCapabilityFailure,
  WT_MAX_LANES,
  type ConnectionValidation,
} from "./paths";
import type {
  PhaseActivity,
  PreparedPaths,
  ProtocolTarget,
  TransferStreamPolicy,
} from "./contract";
import { DEFAULT_CONFIG } from "../state/defaults";
import { testPreparedPaths } from "./test-helpers.testutil";

const auto = { mode: "auto", count: 6 } as const;
const activity = (stage: PhaseActivity["stage"]): PhaseActivity => ({
  stage,
  transfer:
    stage === "download"
      ? ["down"]
      : stage === "upload"
        ? ["up"]
        : ["down", "up"],
  loadedLatency: true,
});
function plan(
  protocol: ProtocolTarget,
  policy: TransferStreamPolicy,
  stage: PhaseActivity["stage"],
  { pings = true, wt = false } = {},
) {
  const paths = testPreparedPaths();
  paths.throughput.fetch = { ...paths.throughput.fetch, protocol };
  if (wt)
    paths.throughput.target = {
      ...paths.throughput.target,
      transport: "webtransport",
      protocol: "http3",
    } as never;
  if (!pings) paths.latency = null;
  const config = { ...DEFAULT_CONFIG, transferStreams: policy };
  return planServerStreams(config, [{ id: "s", paths }], activity(stage)).s;
}

test("automatic streams follow the protocol table, and HTTP/1 reserves control connections", () => {
  expect(plan("http2", auto, "bidirectional")).toEqual({ down: 1, up: 4 });
  expect(plan("http3", auto, "bidirectional")).toEqual({ down: 1, up: 1 });
  expect(plan("http1", auto, "bidirectional")).toEqual({ down: 2, up: 1 });
  expect(plan("negotiated", auto, "download").down).toBe(5);
  expect(plan("negotiated", auto, "upload").up).toBe(3);
  expect(
    plan("http1", { mode: "auto", count: 1 }, "download", { pings: false })
      .down,
  ).toBe(1);
});

test("forced streams are exact per direction, capped only by a session", () => {
  for (const protocol of ["http2", "http3"] as const)
    expect(
      plan(protocol, { mode: "forced", count: 12 }, "bidirectional"),
    ).toEqual({ down: 12, up: 12 });
  expect(
    plan("http3", { mode: "forced", count: 128 }, "download", { wt: true })
      .down,
  ).toBe(WT_MAX_LANES);
  expect(() =>
    plan("http1", { mode: "forced", count: 12 }, "bidirectional"),
  ).toThrow("Forced streams");
});

test("stream diagnostics describe the policy each stage resolves", () => {
  const bidirectional = [activity("bidirectional")];
  const download = [activity("download")];
  const forced = { mode: "forced", count: 128 } as const;
  for (const [policy, stages, protocol, transport, expected] of [
    [
      auto,
      bidirectional,
      "http2",
      undefined,
      "Automatic · 1 download / 4 upload",
    ],
    [
      { mode: "forced", count: 9 },
      bidirectional,
      "http3",
      undefined,
      "Forced · 9 per direction",
    ],
    [
      { mode: "auto", count: 3 },
      download,
      "http1",
      undefined,
      "Automatic · up to 3 per direction",
    ],
    [
      { mode: "auto", count: 4 },
      bidirectional,
      "negotiated",
      undefined,
      "Automatic · up to 2 per direction",
    ],
    [
      forced,
      download,
      "http3",
      "webtransport",
      `Forced · ${WT_MAX_LANES} per direction (capped from 128 by the session)`,
    ],
    [forced, download, "http3", "fetch-stream", "Forced · 128 per direction"],
    [
      forced,
      download,
      "http3",
      "webtransport-datagram",
      "Datagram flood · no lanes",
    ],
  ] as const)
    expect(describeTransferStreams(policy, stages, protocol, transport)).toBe(
      expected,
    );
  for (const [value, expected] of [
    [Number.NaN, 1],
    [0, 1],
    [2.4, 2],
    [999, 128],
  ] as const)
    expect(normalizeStreamCount(value)).toBe(expected);
});

const verified = (paths: PreparedPaths): ConnectionValidation => ({
  throughput: { selection: "auto", state: "verified", path: paths.throughput },
  latency: { selection: "auto", state: "verified", path: paths.latency },
});
const config = () => structuredClone(DEFAULT_CONFIG);

test("equivalent selections and display or stage edits reuse verified paths", () => {
  const paths = testPreparedPaths();
  const validation = verified(paths);
  const edited = config();
  edited.transports.throughputTarget = paths.throughput.target.origin;
  edited.transports.latencyTarget = "transport:websocket";
  edited.visualization.throughputMaxBytesPerSec = 1_000_000;
  edited.duration.downloadMs += 1_000;
  edited.stages.download = false;
  const prepared = preparedPaths(edited, paths.discovery, validation);
  expect(prepared?.throughput).toBe(paths.throughput);
  expect(prepared?.latency).toBe(paths.latency);
  edited.transports.latencyTarget = "https://elsewhere.test";
  expect(
    roleNeedsValidation(edited, validation, "latency", paths.discovery),
  ).toBe(true);
  expect(
    roleNeedsValidation(edited, validation, "throughput", paths.discovery),
  ).toBe(false);
});

test("prepared runs require fresh verified evidence for every needed role", () => {
  const paths = testPreparedPaths();
  const validation = verified(paths);
  expect(preparedPaths(config(), paths.discovery, validation)).not.toBeNull();
  const old = Date.now() - CONNECTION_FRESH_MS - 1_000;
  for (const role of ["throughput", "latency"] as const) {
    const path = { ...validation[role].path!, verifiedAt: old };
    const aged = { ...validation, [role]: { ...validation[role], path } };
    expect(preparedPaths(config(), paths.discovery, aged)).toBeNull();
  }
  for (const state of ["stale", "checking", "failed"] as const) {
    const unverified = {
      ...validation,
      throughput: { ...validation.throughput, state },
    };
    expect(preparedPaths(config(), paths.discovery, unverified)).toBeNull();
  }
  expect(preparedPaths(config(), null, validation)).toBeNull();
});

test("old evidence needs a check after a target descriptor or generation change", () => {
  const paths = testPreparedPaths();
  const validation = verified(paths);
  const changed = structuredClone(paths.discovery);
  const origin = paths.throughput.target.origin;
  changed.throughput[origin].targets[0].protocol = "http2";
  const needs = (role: "throughput" | "latency") =>
    roleNeedsValidation(config(), validation, role, changed);
  expect([needs("throughput"), needs("latency")]).toEqual([true, false]);
  changed.generation = "gen-b";
  expect(needs("latency")).toBe(true);
  expect(preparedPaths(config(), changed, validation)).toBeNull();
});

test("role summaries never borrow another role's failure or checking state", () => {
  const paths = testPreparedPaths();
  const failing = verified(paths);
  failing.throughput = { selection: "auto", state: "failed", path: null };
  const servers = new Map([
    ["a", { validation: verified(paths) }],
    ["b", { validation: failing }],
  ]);
  const summary = (role: "throughput" | "latency", ids = ["a", "b"]) =>
    summarizeRoleValidation(role, ids, servers);
  expect(summary("throughput")).toEqual({
    state: "failed",
    verified: 1,
    total: 2,
  });
  expect(summary("latency")).toEqual({
    state: "verified",
    verified: 2,
    total: 2,
  });
  failing.throughput.state = "checking";
  expect(summary("throughput").state).toBe("checking");
  expect(summary("latency", ["a", "missing"])).toEqual({
    state: "stale",
    verified: 1,
    total: 2,
  });
});

test("missing upload checkpoints block prepared paths only while uploads run", () => {
  const paths = testPreparedPaths();
  paths.discovery.uploadCheckpoint = false;
  const validation = verified(paths);
  const cfg = config();
  cfg.stages.upload = cfg.stages.bidirectional = false;
  const key = connectionDraftRoleKey(cfg, "throughput");
  expect(preparedPaths(cfg, paths.discovery, validation)).not.toBeNull();
  cfg.stages.upload = true;
  expect(connectionDraftRoleKey(cfg, "throughput")).not.toBe(key);
  expect(uploadCapabilityFailure(cfg, paths.discovery)).toContain("checkpoint");
  expect(preparedPaths(cfg, paths.discovery, validation)).toBeNull();
  paths.discovery.uploadCheckpoint = true;
  expect(preparedPaths(cfg, paths.discovery, validation)).not.toBeNull();
});
