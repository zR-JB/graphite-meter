import { expect, test } from "bun:test";
import {
  BROWSER_CONNECTION_BUDGET,
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
  expect(normalizeStreamCount(Number.NaN)).toBe(1);
  expect(normalizeStreamCount(999)).toBe(128);
  expect(describeTransferStreams({ mode: "auto", count: 3 }, "http1")).toBe(
    "Automatic · up to 3 per direction",
  );
});
