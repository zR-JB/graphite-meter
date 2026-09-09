import { RunnerCore } from "../runner/core";
import { RealBackend } from "../runner/RealRunner";
import type {
  LiveRunConfig,
  NetworkRunner,
  RunnerConfig,
  RunResult,
} from "../runner/contract";
import { identity } from "./catalog";
import {
  ServerCoordinator,
  type PreparedServer,
  type ParticipantTransport,
} from "./coordinator";
import type { MultiServerResult, ServerFailure } from "./measurement";
import { planServerStreams, validateServerStreams } from "./streamBudget";

type BackendFactory = (
  paths: PreparedServer["paths"],
  count?: ConstructorParameters<typeof RealBackend>[1],
) => ParticipantTransport;

/** A single receiver uses the ordinary runner; only multiple receivers require aggregation. */
export function createServerRunner(
  servers: PreparedServer[],
  focus: string,
  createBackend: BackendFactory = (paths, count) =>
    new RealBackend(paths, count),
): NetworkRunner {
  if (servers.length !== 1)
    return new ServerCoordinator(servers, focus, createBackend);
  const selected = servers[0];
  const planned = [{ id: selected.server.id, paths: selected.paths }];
  const core: RunnerCore = new RunnerCore(
    createBackend(
      selected.paths,
      (activity, dir) =>
        planServerStreams(core.config!, planned, activity, false)[
          selected.server.id
        ][dir],
    ),
  );
  let started = 0;
  let failures: ServerFailure[] = [];
  core.on((event) => {
    if (event.type === "stageSkipped")
      failures.push({
        serverId: selected.server.id,
        stage: event.failure.stage,
        atMs: Math.max(0, performance.now() - started),
        scope: event.failure.stage === "latency" ? "latency" : "throughput",
        reason: event.failure.reason,
        message: event.failure.message,
      });
    if (event.type === "complete") {
      event.result.multiServer = singleServerDetails(
        selected,
        event.result,
        failures,
      );
      event.result.outcome = failures.length ? "partial" : "complete";
    }
  });
  return {
    get phase() {
      return core.phase;
    },
    start(config: RunnerConfig, rtt: number) {
      validateServerStreams(config, planned, false);
      started = performance.now();
      failures = [];
      core.start(config, rtt);
    },
    reconfigure(config: LiveRunConfig) {
      if (core.config)
        validateServerStreams({ ...core.config, ...config }, planned, false);
      core.reconfigure(config);
    },
    abort: () => core.abort(),
    dispose: () => core.dispose(),
    on: (handler) => core.on(handler),
  };
}

/** Preserve catalogue identity in history without inventing multi-receiver measurement windows. */
function singleServerDetails(
  selected: PreparedServer,
  result: RunResult,
  failures: ServerFailure[],
): MultiServerResult {
  const server = identity(selected.server);
  const { throughput, latency } = selected.paths;
  return {
    selection: [server],
    participants: [server.id],
    latencyFocus: server.id,
    intervals: [],
    omittedIntervals: 0,
    failures: [...failures],
    servers: [
      {
        server,
        throughput: {
          origin: throughput.target.origin,
          transport: throughput.target.transport,
          protocol: throughput.fetch.protocol,
          ...(throughput.browserProtocol
            ? { browserProtocol: throughput.browserProtocol }
            : {}),
          clientIpVersion: throughput.probe.clientIpVersion,
        },
        latencyTarget: latency
          ? {
              origin: latency.target.origin,
              transport: latency.target.transport,
            }
          : null,
        latency: result.latency,
        latencyByStage: result.latencyByStage,
        bufferbloat: result.bufferbloat,
        download: result.download,
        upload: result.upload,
        bidirectional: result.bidirectional,
        totalBytes: {
          down:
            (result.download?.totalBytes ?? 0) +
            (result.bidirectional?.down?.totalBytes ?? 0),
          up:
            (result.upload?.totalBytes ?? 0) +
            (result.bidirectional?.up?.totalBytes ?? 0),
        },
      },
    ],
  };
}
