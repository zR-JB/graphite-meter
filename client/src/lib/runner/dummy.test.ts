import { expect, test } from "bun:test";
import type { CoreHost } from "./core";
import type { RunnerConfig, RunnerEvent } from "./contract";
import { DummyBackend } from "./dummy";

class Host implements CoreHost {
  config: RunnerConfig | null = null;
  phase = "download" as const;
  elapsed = 100;
  events: RunnerEvent[] = [];
  throughput: number[] = [];
  latency = 0;
  latencyObservations: Array<{ observedAtMs: number }> = [];

  ingestThroughput(_dir: "down" | "up", rate: number): void {
    this.throughput.push(rate);
  }
  ingestLatency(sample: { rttMs: number; observedAtMs: number }): void {
    this.latency = sample.rttMs;
    this.latencyObservations.push(sample);
  }
  recordRecoveryGap(): void {}
  recordRecoveryBytes(): void {}
  stall(): void {}
  resume(): void {}
  emit(event: RunnerEvent): void {
    this.events.push(event);
  }
  fail(): void {}
  failStage(): void {}
  presentationRate(): number {
    return 0;
  }
}

const activity = {
  stage: "download" as const,
  transfer: ["down" as const],
  loadedLatency: true,
};

test("probe publishes a complete browser-fixture connection description", async () => {
  const backend = new DummyBackend();
  const host = new Host();
  backend.attach(host);
  const info = await backend.probe({
    transports: { throughputTarget: "auto", latencyTarget: "auto" },
  } as RunnerConfig);

  expect(info.engineVersion).toBe("browser-fixture");
  expect(info.selectedThroughputTarget).toBe("dummy-fetch");
  const event = host.events[0];
  expect(event?.type).toBe("transportDiscovery");
  if (event?.type !== "transportDiscovery") return;
  const origin = Object.keys(event.discovery.throughput)[0];
  expect(origin).toBeDefined();
  expect(
    event.discovery.throughput[origin!].targets.map(
      (target) => target.transport,
    ),
  ).toEqual(["fetch-stream", "webtransport", "webtransport-datagram"]);
  expect(
    event.discovery.latency[origin!].targets.map((target) => target.transport),
  ).toEqual(["websocket", "webtransport"]);
});

test("measured stages emit stable throughput and loaded latency samples", async () => {
  const backend = new DummyBackend();
  const host = new Host();
  backend.attach(host);
  backend.onStageBegin(activity);
  backend.onStageMeasure(activity);
  await new Promise((resolve) => setTimeout(resolve, 75));
  backend.onStageEnd();

  expect(host.throughput.length).toBeGreaterThan(0);
  expect(host.throughput[0]).toBe(40_000_000);
  expect(host.latency).toBe(16);
  expect(host.latencyObservations[0]?.observedAtMs).not.toBe(host.elapsed);
});
