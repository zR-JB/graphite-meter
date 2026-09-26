import { expect, test } from "bun:test";
import {
  describeTransferStreams,
  normalizeStreamCount,
  planServerStreams,
  WT_MAX_LANES,
} from "./paths";
import type {
  PhaseActivity,
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
