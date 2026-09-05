/* Deterministic backend used only by the browser-test build. */

import type { CoreHost, RunnerBackend } from "../src/lib/runner/core";
import type {
  EngineInfo,
  InfraInfo,
  PhaseActivity,
  RunnerConfig,
  TransportDiscovery,
} from "../src/lib/runner/contract";
import { classifyTransportDiscovery } from "../src/lib/runner/real/backendPure";

const DOWN_RATE = 40_000_000;
const UP_RATE = 8_000_000;
const RTT_MS = 16;
const TICK_MS = 60;
const FETCH = "dummy-fetch";
const WT = "dummy-webtransport";
const DATAGRAM = "dummy-datagram";
const WS = "dummy-websocket";

export class DummyBackend implements RunnerBackend {
  #host: CoreHost | null = null;
  #activity: PhaseActivity | null = null;
  #timer: ReturnType<typeof setTimeout> | null = null;

  attach(host: CoreHost): void {
    this.#host = host;
  }

  describe(): EngineInfo {
    return {
      name: "dummy",
      version: "browser-fixture",
      latencyTransports: ["webtransport", "websocket"],
      throughputTransports: [
        "fetch-stream",
        "webtransport",
        "webtransport-datagram",
      ],
    };
  }

  async probe(config: RunnerConfig, signal?: AbortSignal): Promise<InfraInfo> {
    signal?.throwIfAborted();
    const origin =
      typeof location === "undefined" ? "http://dummy.test" : location.origin;
    const tls = origin.startsWith("https:");
    const discovery = {
      ...classifyTransportDiscovery(
        [
          { baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" },
          { baseUrl: ".", transport: "webtransport", protocol: "http3" },
          {
            baseUrl: ".",
            transport: "webtransport-datagram",
            protocol: "http3",
          },
        ],
        [
          { baseUrl: ".", transport: "websocket" },
          { baseUrl: ".", transport: "webtransport" },
        ],
        origin,
        tls,
        "http/1.1",
      ),
      generation: "dummy-browser",
      engineVersion: "browser-fixture",
      server: { name: "Graphite Meter browser fixture", location: "test" },
      fetchedAt: Date.now(),
    } satisfies TransportDiscovery;
    for (const target of discovery.throughput[origin].targets)
      target.id =
        target.transport === "fetch-stream"
          ? FETCH
          : target.transport === "webtransport"
            ? WT
            : DATAGRAM;
    for (const target of discovery.latency[origin].targets)
      target.id = target.transport === "websocket" ? WS : WT;
    this.#host?.emit({ type: "transportDiscovery", discovery });

    const throughputId = [FETCH, WT, DATAGRAM].includes(
      config.transports.throughputTarget,
    )
      ? config.transports.throughputTarget
      : FETCH;
    const throughput = discovery.throughput[origin].targets.find(
      (target) => target.id === throughputId,
    );
    const latencyId =
      config.transports.latencyTarget === WT ||
      (config.transports.latencyTarget === "auto" &&
        typeof WebTransport !== "undefined")
        ? WT
        : WS;
    const latency = discovery.latency[origin].targets.find(
      (target) => target.id === latencyId,
    );
    if (!throughput || !latency)
      throw new Error("dummy target selection failed");
    return {
      // Fixture uses TEST-NET as a remote Ethernet path; loopback is unit-tested.
      clientIp: "192.0.2.1",
      clientIpVersion: 4,
      clientIpSource: "socket",
      server: discovery.server,
      preTestPingMs: RTT_MS,
      engineVersion: discovery.engineVersion,
      discoveryGeneration: discovery.generation,
      protocolNegotiated: throughput.protocol === "http3" ? "h3" : "http/1.1",
      selectedThroughputTarget: throughputId,
      selectedThroughputProtocol:
        throughput.protocol === "negotiated" ? "http1" : throughput.protocol,
      selectedThroughputTransport: throughput.transport,
      selectedLatencyTarget: latencyId,
      selectedLatencyTransport: latency.transport,
      latencyProtocolNegotiated:
        latency.protocol === "http3" ? "h3" : "http/1.1",
      firstHopProtocol: "http/1.1",
      firstHopSecure: tls,
      serverLoad: { active: 0, max: 1 },
    };
  }

  onRunStart(): void {}
  onStageBegin(activity: PhaseActivity): void {
    this.#activity = activity;
    this.#stop();
  }
  onStageMeasure(activity: PhaseActivity): void {
    this.#activity = activity;
    this.#stop();
    this.#scheduleSample();
  }
  onStageEnd(): void {
    this.#stop();
    this.#activity = null;
  }
  onComplete(): void {
    this.#stop();
  }
  onAbort(): void {
    this.#stop();
  }
  idleHintMs(): number {
    return RTT_MS;
  }

  #scheduleSample(): void {
    this.#timer = setTimeout(() => {
      this.#timer = null;
      this.#sample();
      if (this.#activity) this.#scheduleSample();
    }, TICK_MS);
  }
  #sample(): void {
    const activity = this.#activity;
    const host = this.#host;
    if (!activity || !host) return;
    const seconds = TICK_MS / 1000;
    for (const direction of activity.transfer) {
      const rate = direction === "down" ? DOWN_RATE : UP_RATE;
      host.ingestThroughput(direction, rate, rate * seconds, seconds);
    }
    if (!activity.transfer.length || activity.loadedLatency)
      host.ingestLatency({
        rttMs: RTT_MS,
        lost: false,
        observedAtMs: performance.now(),
      });
  }
  #stop(): void {
    if (this.#timer !== null) clearTimeout(this.#timer);
    this.#timer = null;
  }
}
