import { expect, test } from "bun:test";
import type { CoreHost } from "./core";
import type { RunnerConfig, RunnerEvent } from "./contract";
import { emptyConnectionValidation } from "./connectionModel";
import { DummyBackend } from "./dummy";

const noop = () => {};
class Host implements CoreHost {
  ingestLatencyAccountingIncomplete = noop;
  ingestLatencyInterruption = noop;
  config: RunnerConfig | null = null;
  phase = "download" as const;
  elapsed = 100;
  events: RunnerEvent[] = [];
  throughput: number[] = [];
  latency = 0;
  latencyObservations: Array<{ observedAtMs: number }> = [];

  ingestThroughput(_dir: "down" | "up", bytes: number, seconds: number): void {
    this.throughput.push(bytes / seconds);
  }
  ingestLatency(sample: { rttMs: number; observedAtMs: number }): void {
    this.latency = sample.rttMs;
    this.latencyObservations.push(sample);
  }
  recordRecoveryGap = noop;
  recordRecoveryBytes = noop;
  stall = noop;
  resume = noop;
  emit(event: RunnerEvent): void {
    this.events.push(event);
  }
  fail = noop;
  failStage = noop;
  presentationRate = () => 0;
}

const activity = {
  stage: "download" as const,
  transfer: ["down" as const],
  loadedLatency: true,
};

test("probe publishes a complete browser-fixture connection description", async () => {
  const { discovery, validation } = await DummyBackend.prepare(
    {
      transports: { throughputTarget: "auto", latencyTarget: "auto" },
    } as RunnerConfig,
    emptyConnectionValidation(),
    ["throughput", "latency"],
    new AbortController().signal,
  );
  expect(discovery.engineVersion).toBe("browser-fixture");
  expect(validation.throughput.path?.target.id).toBe("dummy-fetch");
  const origin = Object.keys(discovery.throughput)[0];
  expect(origin).toBeDefined();
  expect(
    discovery.throughput[origin!].targets.map((target) => target.transport),
  ).toEqual(["fetch-stream", "webtransport", "webtransport-datagram"]);
  expect(
    discovery.latency[origin!].targets.map((target) => target.transport),
  ).toEqual(["websocket", "webtransport"]);
});

test("measured stages emit stable throughput and loaded latency samples", async () => {
  const backend = new DummyBackend();
  const host = new Host();
  backend.attach(host);
  backend.onStageBegin(activity);
  backend.onStageMeasure(activity);
  await new Promise<void>((resolve) => setTimeout(resolve, 75));
  backend.onStageEnd();

  expect(host.throughput.length).toBeGreaterThan(0);
  expect(host.throughput[0]).toBe(40_000_000);
  expect(host.latency).toBe(16);
  expect(host.latencyObservations[0]?.observedAtMs).not.toBe(host.elapsed);
});
