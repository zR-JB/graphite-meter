/* Small deterministic backend used only by the browser-test build. */

import type {
  EngineInfo,
  InfraInfo,
  PhaseActivity,
  RunnerConfig,
  TransportDiscovery,
} from "./contract";
import type { CoreHost, RunnerBackend } from "./core";
import { ROUTES } from "./real/backendPure";

const DOWN_RATE = 40_000_000;
const UP_RATE = 8_000_000;
const RTT_MS = 16;
const TICK_MS = 60;
const DUMMY_FETCH_ID = "dummy-fetch";
const DUMMY_WEBTRANSPORT_ID = "dummy-webtransport";
const DUMMY_DATAGRAM_ID = "dummy-datagram";
const DUMMY_WEBSOCKET_ID = "dummy-websocket";

// Browser-only stable samples exercise RunnerCore without a running server or transport implementation.
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
    const secure = origin.startsWith("https:");
    const fetchTarget = {
      id: DUMMY_FETCH_ID,
      origin,
      transport: "fetch-stream" as const,
      protocol: "negotiated" as const,
      tls: secure,
      routes: {
        probe: ROUTES.probe,
        download: ROUTES.download,
        upload: ROUTES.upload,
        uploadSession: ROUTES.uploadSession,
        uploadProgress: ROUTES.uploadProgress,
      },
    };
    const webTransportRoutes = {
      probe: ROUTES.probe,
      wtSession: ROUTES.wtSession,
      wtDownload: ROUTES.wtDownload,
      wtUpload: ROUTES.wtUpload,
      uploadSession: ROUTES.uploadSession,
      uploadProgress: ROUTES.uploadProgress,
    };
    const webTransportTarget = {
      id: DUMMY_WEBTRANSPORT_ID,
      origin,
      transport: "webtransport" as const,
      protocol: "http3" as const,
      tls: secure,
      routes: webTransportRoutes,
    };
    const datagramTarget = {
      id: DUMMY_DATAGRAM_ID,
      origin,
      transport: "webtransport-datagram" as const,
      protocol: "http3" as const,
      tls: secure,
      routes: webTransportRoutes,
    };
    const websocketTarget = {
      id: DUMMY_WEBSOCKET_ID,
      origin,
      transport: "websocket" as const,
      protocol: "http1" as const,
      tls: secure,
      routes: { probe: ROUTES.probe, ping: ROUTES.ping },
    };
    const discovery: TransportDiscovery = {
      generation: "dummy-browser",
      engineVersion: "browser-fixture",
      server: { name: "Graphite Meter browser fixture", location: "test" },
      fetchedAt: Date.now(),
      pageOrigin: origin,
      pageSecure: origin.startsWith("https:"),
      throughput: {
        [origin]: {
          state: "advertised",
          targets: [fetchTarget, webTransportTarget, datagramTarget],
        },
      },
      latency: {
        [origin]: {
          state: "advertised",
          targets: [
            websocketTarget,
            {
              id: DUMMY_WEBTRANSPORT_ID,
              origin,
              transport: "webtransport" as const,
              protocol: "http3" as const,
              tls: secure,
              routes: {
                probe: ROUTES.probe,
                wtSession: ROUTES.wtSession,
                wtPing: ROUTES.wtPing,
              },
            },
          ],
        },
      },
    };
    this.#host?.emit({ type: "transportDiscovery", discovery });
    const throughputSelection = config.transports.throughputTarget;
    const throughputTarget = [
      DUMMY_FETCH_ID,
      DUMMY_WEBTRANSPORT_ID,
      DUMMY_DATAGRAM_ID,
    ].includes(throughputSelection)
      ? throughputSelection
      : DUMMY_FETCH_ID;
    const throughput = discovery.throughput[origin].targets.find(
      (target) => target.id === throughputTarget,
    )!;
    const latencySelection = config.transports.latencyTarget;
    const latencyTarget =
      latencySelection === DUMMY_WEBTRANSPORT_ID ||
      (latencySelection === "auto" && typeof WebTransport !== "undefined")
        ? DUMMY_WEBTRANSPORT_ID
        : DUMMY_WEBSOCKET_ID;
    const latency = discovery.latency[origin].targets.find(
      (target) => target.id === latencyTarget,
    )!;
    return {
      clientIp: "127.0.0.1",
      clientIpVersion: 4,
      clientIpSource: "socket",
      server: discovery.server,
      preTestPingMs: RTT_MS,
      engineVersion: discovery.engineVersion,
      discoveryGeneration: discovery.generation,
      protocolNegotiated: throughput.protocol === "http3" ? "h3" : "http/1.1",
      selectedThroughputTarget: throughputTarget,
      selectedThroughputProtocol: throughput.protocol,
      selectedThroughputTransport: throughput.transport,
      selectedLatencyTarget: latencyTarget,
      selectedLatencyTransport: latency.transport,
      latencyProtocolNegotiated:
        latency.protocol === "http3" ? "h3" : "http/1.1",
      firstHopProtocol: "http/1.1",
      firstHopSecure: discovery.pageSecure,
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
    if (activity.transfer.length) {
      for (const direction of activity.transfer) {
        const rate = direction === "down" ? DOWN_RATE : UP_RATE;
        host.ingestThroughput(
          direction,
          rate,
          rate * (TICK_MS / 1000),
          TICK_MS / 1000,
        );
      }
    }
    if (!activity.transfer.length || activity.loadedLatency)
      host.ingestLatency(
        { rttMs: RTT_MS, lost: false, observedAtMs: performance.now() },
        activity.loadedLatency,
      );
  }

  #stop(): void {
    if (this.#timer !== null) clearTimeout(this.#timer);
    this.#timer = null;
  }
}
