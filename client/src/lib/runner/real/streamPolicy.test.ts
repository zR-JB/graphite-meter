import { expect, test } from "bun:test";
import {
  BROWSER_CONNECTION_BUDGET,
  WT_MAX_LANES,
  HTTP3_DOWNLOAD_STREAMS,
  MULTIPLEXED_UPLOAD_STREAMS,
  describeTransferStreams,
  normalizeStreamCount,
  transferStreamCount,
} from "./streamPolicy";

const auto = { mode: "auto", count: 6 } as const;

test("automatic multiplexed streams avoid per-stream transfer stalls", () => {
  const base = {
    policy: auto,
    transfer: ["down", "up"],
    needsPing: true,
  } as const;
  expect(transferStreamCount({ ...base, protocol: "http2", dir: "down" })).toBe(
    1,
  );
  expect(transferStreamCount({ ...base, protocol: "http3", dir: "down" })).toBe(
    HTTP3_DOWNLOAD_STREAMS,
  );
  for (const protocol of ["http2", "http3"] as const)
    expect(transferStreamCount({ ...base, protocol, dir: "up" })).toBe(
      MULTIPLEXED_UPLOAD_STREAMS,
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
  expect(describeTransferStreams(auto, "http3")).toBe(
    `Automatic · ${HTTP3_DOWNLOAD_STREAMS} download / ${MULTIPLEXED_UPLOAD_STREAMS} upload`,
  );
  expect(describeTransferStreams({ mode: "forced", count: 9 }, "http3")).toBe(
    "Forced · 9 per direction",
  );
  expect(describeTransferStreams({ mode: "auto", count: 3 }, "http1")).toBe(
    "Automatic · up to 3 per direction",
  );
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
  expect(describeTransferStreams(policy, "http3", true)).toBe(
    `Forced · ${WT_MAX_LANES} per direction (capped from 128 by the session)`,
  );

  // Below the cap it is exact, and fetch lanes are untouched.
  const modest = { mode: "forced" as const, count: 4 };
  expect(describeTransferStreams(modest, "http3", true)).toBe(
    "Forced · 4 per direction",
  );
  expect(describeTransferStreams(policy, "http3", false)).toBe(
    "Forced · 128 per direction",
  );
});
