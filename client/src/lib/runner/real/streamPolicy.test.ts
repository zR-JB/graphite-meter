import { expect, test } from "bun:test";
import {
  BROWSER_CONNECTION_BUDGET,
  WT_MAX_LANES,
  MULTIPLEXED_STREAMS,
  describeTransferStreams,
  normalizeStreamCount,
  transferStreamCount,
} from "./streamPolicy";
import type { FlowDirection, PhaseActivity } from "../contract";

const auto = { mode: "auto", count: 6 } as const;

const stage = (
  stage: PhaseActivity["stage"],
  transfer: FlowDirection[],
): PhaseActivity => ({ stage, transfer, loadedLatency: true });

const download = [stage("download", ["down"])];
const bidirectional = [stage("bidirectional", ["down", "up"])];

// One multiplexed download stream already runs at the connection rate. Upload
// splits by protocol: h2 wants lanes, h3 measures higher on one.
test("automatic multiplexed streams follow the per-protocol table", () => {
  const base = {
    policy: auto,
    transfer: ["down", "up"],
    needsPing: true,
  } as const;
  expect(MULTIPLEXED_STREAMS.http2).toEqual({ down: 1, up: 4 });
  expect(MULTIPLEXED_STREAMS.http3).toEqual({ down: 1, up: 1 });
  for (const protocol of ["http2", "http3"] as const)
    for (const dir of ["down", "up"] as const)
      expect(transferStreamCount({ ...base, protocol, dir })).toBe(
        MULTIPLEXED_STREAMS[protocol][dir],
      );
});

test("automatic H1 reserves control connections and splits bidirectional capacity", () => {
  const base = {
    protocol: "http1",
    policy: auto,
    transfer: ["down", "up"],
    needsPing: true,
  } as const;
  expect(transferStreamCount({ ...base, dir: "down" })).toBe(2);
  expect(transferStreamCount({ ...base, dir: "up" })).toBe(2);
  expect(
    transferStreamCount({
      protocol: "http1",
      policy: auto,
      transfer: ["down"],
      dir: "down",
      needsPing: true,
    }),
  ).toBe(BROWSER_CONNECTION_BUDGET - 1);
  expect(
    transferStreamCount({
      protocol: "http1",
      policy: { mode: "auto", count: 1 },
      transfer: ["down"],
      dir: "down",
      needsPing: false,
    }),
  ).toBe(1);
});

test("unobserved negotiated HTTP uses the H1-safe connection budget", () => {
  const oneWay = {
    protocol: "negotiated",
    policy: auto,
    needsPing: true,
  } as const;
  expect(
    transferStreamCount({ ...oneWay, transfer: ["down"], dir: "down" }),
  ).toBe(BROWSER_CONNECTION_BUDGET - 1);
  expect(transferStreamCount({ ...oneWay, transfer: ["up"], dir: "up" })).toBe(
    BROWSER_CONNECTION_BUDGET - 2,
  );

  const bidirectional = { ...oneWay, transfer: ["down", "up"] } as const;
  expect(transferStreamCount({ ...bidirectional, dir: "down" })).toBe(2);
  expect(transferStreamCount({ ...bidirectional, dir: "up" })).toBe(2);
});

test("forced policy is exact per direction and ignores protocol and browser budget", () => {
  for (const protocol of ["http1", "http2", "http3"] as const) {
    expect(
      transferStreamCount({
        protocol,
        policy: { mode: "forced", count: 12 },
        transfer: ["down", "up"],
        dir: "up",
        needsPing: true,
        totalBudget: 2,
      }),
    ).toBe(12);
  }
});

test("stream diagnostics distinguish automatic and forced policy", () => {
  expect(describeTransferStreams(auto, bidirectional, "http2")).toBe(
    "Automatic · 1 download / 4 upload",
  );
  expect(describeTransferStreams(auto, bidirectional, "http3")).toBe(
    "Automatic · 1 download / 1 upload",
  );
  expect(
    describeTransferStreams(
      { mode: "forced", count: 9 },
      bidirectional,
      "http3",
    ),
  ).toBe("Forced · 9 per direction");
  expect(
    describeTransferStreams({ mode: "auto", count: 3 }, download, "http1"),
  ).toBe("Automatic · up to 3 per direction");
});

// The endpoint panel and the copyable report show this string while the lanes
// come from transferStreamCount, so a stage that splits the H1 budget must not
// be described with the count a download-only stage would get.
test("the automatic H1 description reports the count its stages resolve", () => {
  const policy = { mode: "auto", count: 4 } as const;
  for (const protocol of ["http1", "negotiated"] as const) {
    const count = transferStreamCount({
      protocol,
      policy,
      transfer: bidirectional[0].transfer,
      dir: "down",
      needsPing: true,
    });
    expect(count).toBe(2);
    expect(describeTransferStreams(policy, bidirectional, protocol)).toBe(
      `Automatic · up to ${count} per direction`,
    );
    // A download-only run keeps the wider count: only the split is narrower.
    expect(describeTransferStreams(policy, download, protocol)).toBe(
      "Automatic · up to 4 per direction",
    );
  }
});

test("stream counts are clamped to a usable range", () => {
  expect(normalizeStreamCount(Number.NaN)).toBe(1);
  expect(normalizeStreamCount(0)).toBe(1);
  expect(normalizeStreamCount(2.4)).toBe(2);
  expect(normalizeStreamCount(999)).toBe(128);
});

// A WebTransport session delivers at most WT_MAX_LANES per direction, so a
// higher forced count must be reported as what the transport carries. Claiming
// a lane count the session never opened misdescribes the measurement.
test("a forced count above the session cap is clamped and said so", () => {
  const policy = { mode: "forced" as const, count: 128 };
  expect(
    transferStreamCount({
      protocol: "http3",
      policy,
      transfer: ["down"],
      dir: "down",
      needsPing: false,
      webTransport: true,
    }),
  ).toBe(WT_MAX_LANES);
  expect(
    describeTransferStreams(policy, download, "http3", "webtransport"),
  ).toBe(
    `Forced · ${WT_MAX_LANES} per direction (capped from 128 by the session)`,
  );

  // Below the cap it is exact, and fetch lanes are untouched.
  const modest = { mode: "forced" as const, count: 4 };
  expect(
    describeTransferStreams(modest, download, "http3", "webtransport"),
  ).toBe("Forced · 4 per direction");
  expect(
    describeTransferStreams(policy, download, "http3", "fetch-stream"),
  ).toBe("Forced · 128 per direction");

  // A datagram run opens no lanes at all, so no lane count describes it.
  expect(
    describeTransferStreams(policy, download, "http3", "webtransport-datagram"),
  ).toBe("Datagram flood · no lanes");
});
