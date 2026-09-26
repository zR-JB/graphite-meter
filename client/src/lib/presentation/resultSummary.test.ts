import { expect, test } from "bun:test";
import type { MultiServerResult } from "../servers/measurement";
import { serverEvidence, summaryCards } from "./resultSummary";

const lane = (reportedBytesPerSec: number) => ({
  reportedBytesPerSec,
  totalBytes: 1_000_000,
  stabilityPct: 95,
});
const rate = (value: number) => ({ num: String(value), unit: "B/s" });

test("a one-lane bidirectional result has no combined value, only its surviving lane", () => {
  const [card] = summaryCards(
    {
      status: { bidirectional: "partial" },
      download: null,
      upload: null,
      bidirectional: { down: lane(40), up: null },
      latency: null,
      latencyMeasured: true,
      wire: {},
    },
    rate,
    "base10",
  );
  expect(card).toMatchObject({
    num: "—",
    quality: null,
    detail: "↓ 40 B/s · upload unavailable",
  });
});

test("a server's own failures decide its statuses; a server without a latency path is unmeasured", () => {
  const server = (id: string, latency: boolean) => ({
    server: { id, name: id, url: `https://${id}.test` },
    throughput: { origin: "", transport: "fetch-stream", protocol: "http1" },
    latencyTarget: latency ? { origin: "", transport: "websocket" } : null,
    latency: null,
    latencyByStage: {
      latency: null,
      download: null,
      upload: null,
      bidirectional: null,
    },
    bufferbloat: null,
    download: lane(10) as never,
    upload: null,
    bidirectional: null,
    totalBytes: { down: 1, up: 0 },
  });
  const details = {
    servers: [server("a", true), server("b", false)],
    failures: [
      {
        serverId: "a",
        stage: "upload",
        scope: "throughput",
        atMs: 0,
        reason: "",
        message: "",
      },
    ],
  } as unknown as MultiServerResult;
  const status = {
    download: "complete",
    upload: "complete",
    latency: "complete",
  } as const;
  expect(serverEvidence(details, "a", status)?.status).toEqual({
    latency: "partial",
    download: "complete",
    upload: "failed",
  });
  const b = serverEvidence(details, "b", status)!;
  expect(b.status).toEqual({
    latency: "complete",
    download: "complete",
    upload: "partial",
  });
  expect(summaryCards(b, rate, "base10").at(-1)).toMatchObject({
    num: "—",
    detail: "Not measured",
  });
});
