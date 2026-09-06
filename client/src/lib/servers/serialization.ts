import { canonicalOrigin } from "./catalog";
import type { MultiServerResult } from "./measurement";
import {
  hasLatencyMeasurements,
  hasThroughputMeasurements,
  isReflectorTimingSummary,
} from "../history/measurementValidation";

const stages = ["latency", "download", "upload", "bidirectional"];
const object = (value: unknown): value is Record<string, unknown> =>
  !!value && typeof value === "object" && !Array.isArray(value);
const number = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value) && value >= 0;
const text = (value: unknown, max = 256): value is string =>
  typeof value === "string" && value.length <= max;
const keys = (value: Record<string, unknown>, allowed: string[]) =>
  Object.keys(value).every((key) => allowed.includes(key));
const list = (value: unknown, max: number): value is unknown[] =>
  Array.isArray(value) && value.length <= max;
function origin(value: unknown) {
  try {
    return canonicalOrigin(value) === value;
  } catch {
    return false;
  }
}
function optionalNumbers(value: Record<string, unknown>, fields: string[]) {
  return fields.every((key) => value[key] === null || number(value[key]));
}
function throughput(value: unknown) {
  return (
    value === null ||
    (object(value) &&
      keys(value, [
        "peakBytesPerSec",
        "stabilityPct",
        "totalBytes",
        "reportedBytesPerSec",
        "fullAverageBytesPerSec",
        "method",
        "stabilityScore",
        "band",
        "probeTimeoutPct",
        "serverAuthoritative",
      ]) &&
      hasThroughputMeasurements(value) &&
      (value.serverAuthoritative === undefined ||
        typeof value.serverAuthoritative === "boolean"))
  );
}
function latency(value: unknown) {
  return (
    value === null ||
    (object(value) &&
      keys(value, [
        "idleMs",
        "minMs",
        "p50Ms",
        "p95Ms",
        "jitterMs",
        "probeTimeoutPct",
        "reportedMs",
        "method",
        "stabilityScore",
        "band",
      ]) &&
      number(value.idleMs) &&
      hasLatencyMeasurements(value))
  );
}
function grade(value: unknown) {
  return (
    value === null ||
    (object(value) &&
      keys(value, ["grade", "idleMs", "loadedMs", "increaseMs"]) &&
      ["A", "B", "C", "D", "F"].includes(String(value.grade)) &&
      ["idleMs", "loadedMs", "increaseMs"].every((key) => number(value[key])))
  );
}
function latencySummary(value: unknown) {
  if (value === null) return true;
  if (
    !object(value) ||
    !keys(value, [
      "reflectorTiming",
      "accountingComplete",
      "probeCount",
      "timeoutCount",
      "unresolvedCount",
      "sendFailureCount",
      "jitterPairs",
      "minMs",
      "maxMs",
      "meanMs",
      "p10Ms",
      "p50Ms",
      "p90Ms",
      "p95Ms",
      "jitterMs",
    ]) ||
    typeof value.accountingComplete !== "boolean"
  )
    return false;
  if (
    ![
      "probeCount",
      "timeoutCount",
      "unresolvedCount",
      "sendFailureCount",
      "jitterPairs",
    ].every((key) => Number.isSafeInteger(value[key]) && number(value[key])) ||
    (value.timeoutCount as number) > (value.probeCount as number) ||
    !optionalNumbers(value, [
      "minMs",
      "maxMs",
      "meanMs",
      "p10Ms",
      "p50Ms",
      "p90Ms",
      "p95Ms",
      "jitterMs",
    ])
  )
    return false;
  return (
    value.reflectorTiming === undefined ||
    isReflectorTimingSummary(
      value.reflectorTiming,
      (value.probeCount as number) - (value.timeoutCount as number),
    )
  );
}
function identity(value: unknown) {
  return (
    object(value) &&
    keys(value, ["id", "url", "name", "location"]) &&
    text(value.id, 64) &&
    /^[a-zA-Z0-9._-]+$/.test(value.id) &&
    origin(value.url) &&
    text(value.name) &&
    (value.location === undefined || text(value.location))
  );
}
function uniqueIds(
  value: unknown,
  ids: string[],
  minimum = 0,
): value is string[] {
  return (
    list(value, 4) &&
    value.length >= minimum &&
    new Set(value).size === value.length &&
    value.every((id) => typeof id === "string" && ids.includes(id))
  );
}
function close(a: number, b: number) {
  return Math.abs(a - b) <= 1e-8 * Math.max(1, a, b);
}
function window(value: unknown, ids: string[], stage: string) {
  if (value === null) return true;
  if (
    !object(value) ||
    !keys(value, [
      "startMs",
      "endMs",
      "down",
      "up",
      "downBytesPerSec",
      "upBytesPerSec",
    ]) ||
    !number(value.startMs) ||
    !number(value.endMs) ||
    value.endMs <= value.startMs
  )
    return false;
  for (const dir of ["down", "up"]) {
    const enabled =
      stage === "bidirectional" ||
      stage === (dir === "down" ? "download" : "upload");
    const components = value[dir],
      rate = value[`${dir}BytesPerSec`];
    if (!enabled) {
      if (components !== null || rate !== null) return false;
      continue;
    }
    if (
      !list(components, 4) ||
      components.length !== ids.length ||
      !uniqueIds(
        components.map((component) =>
          object(component) ? component.serverId : null,
        ),
        ids,
        ids.length,
      ) ||
      !number(rate)
    )
      return false;
    let sum = 0;
    for (const component of components) {
      if (
        !object(component) ||
        !keys(component, [
          "serverId",
          "bytes",
          "durationMs",
          "bytesPerSec",
          "clock",
          "startBytes",
          "endBytes",
          "startNanos",
          "endNanos",
          "startRequestMs",
          "startResponseMs",
          "endRequestMs",
          "endResponseMs",
        ]) ||
        !["bytes", "durationMs", "bytesPerSec", "startBytes", "endBytes"].every(
          (key) => number(component[key]),
        ) ||
        (component.durationMs as number) <= 0 ||
        !close(
          (component.endBytes as number) - (component.startBytes as number),
          component.bytes as number,
        ) ||
        !close(
          ((component.bytes as number) * 1000) /
            (component.durationMs as number),
          component.bytesPerSec as number,
        )
      )
        return false;
      if (dir === "up") {
        if (
          component.clock !== "receiver" ||
          ![
            "startNanos",
            "endNanos",
            "startRequestMs",
            "startResponseMs",
            "endRequestMs",
            "endResponseMs",
          ].every((key) => number(component[key])) ||
          !close(
            ((component.endNanos as number) -
              (component.startNanos as number)) /
              1e6,
            component.durationMs as number,
          )
        )
          return false;
      } else if (
        component.clock !== "client-monotonic" ||
        !close(value.endMs - value.startMs, component.durationMs as number)
      )
        return false;
      sum += component.bytesPerSec as number;
    }
    if (!close(sum, rate)) return false;
  }
  return true;
}
/** Imported history is bounded and checked before any renderer sees server or clock metadata. */
export function isMultiServerResult(
  value: unknown,
): value is MultiServerResult {
  if (
    !object(value) ||
    !keys(value, [
      "selection",
      "participants",
      "latencyFocus",
      "servers",
      "intervals",
      "omittedIntervals",
      "failures",
    ]) ||
    !list(value.selection, 4) ||
    value.selection.length < 1 ||
    !value.selection.every(identity)
  )
    return false;
  const selection = value.selection as Record<string, unknown>[],
    ids = selection.map((server) => server.id as string);
  if (
    new Set(ids).size !== ids.length ||
    !uniqueIds(value.participants, ids) ||
    !ids.includes(String(value.latencyFocus)) ||
    !list(value.servers, 4) ||
    value.servers.length !== ids.length ||
    !Number.isSafeInteger(value.omittedIntervals) ||
    !number(value.omittedIntervals) ||
    !list(value.intervals, 128) ||
    !list(value.failures, 32)
  )
    return false;
  const seen = new Set<string>();
  for (const server of value.servers) {
    if (
      !object(server) ||
      !keys(server, [
        "server",
        "throughput",
        "latencyTarget",
        "latency",
        "latencyByStage",
        "bufferbloat",
        "download",
        "upload",
        "bidirectional",
        "totalBytes",
      ]) ||
      !identity(server.server)
    )
      return false;
    const identityValue = server.server as Record<string, unknown>,
      id = identityValue.id as string;
    if (
      !ids.includes(id) ||
      seen.has(id) ||
      identityValue.url !== selection.find((server) => server.id === id)?.url
    )
      return false;
    seen.add(id);
    if (
      !object(server.throughput) ||
      !keys(server.throughput, [
        "origin",
        "transport",
        "protocol",
        "browserProtocol",
        "clientIpVersion",
      ]) ||
      !origin(server.throughput.origin) ||
      (server.throughput.browserProtocol !== undefined &&
        !text(server.throughput.browserProtocol, 64)) ||
      (server.throughput.clientIpVersion !== undefined &&
        ![4, 6].includes(Number(server.throughput.clientIpVersion))) ||
      !["fetch-stream", "webtransport", "webtransport-datagram"].includes(
        String(server.throughput.transport),
      ) ||
      !["http1", "http2", "http3", "negotiated"].includes(
        String(server.throughput.protocol),
      )
    )
      return false;
    if (
      server.latencyTarget !== null &&
      (!object(server.latencyTarget) ||
        !keys(server.latencyTarget, ["origin", "transport"]) ||
        !origin(server.latencyTarget.origin) ||
        !["websocket", "webtransport"].includes(
          String(server.latencyTarget.transport),
        ))
    )
      return false;
    if (
      !latency(server.latency) ||
      !grade(server.bufferbloat) ||
      !throughput(server.download) ||
      !throughput(server.upload) ||
      !object(server.totalBytes) ||
      !keys(server.totalBytes, ["down", "up"]) ||
      !number(server.totalBytes.down) ||
      !number(server.totalBytes.up) ||
      !object(server.latencyByStage) ||
      !keys(server.latencyByStage, stages) ||
      !stages.every((stage) =>
        latencySummary(
          (server.latencyByStage as Record<string, unknown>)[stage],
        ),
      )
    )
      return false;
    if (
      server.bidirectional !== null &&
      (!object(server.bidirectional) ||
        !keys(server.bidirectional, ["down", "up"]) ||
        !throughput(server.bidirectional.down) ||
        !throughput(server.bidirectional.up))
    )
      return false;
  }
  for (const [index, interval] of value.intervals.entries()) {
    if (
      !object(interval) ||
      !keys(interval, [
        "id",
        "stage",
        "participants",
        "startMs",
        "endMs",
        "complete",
        "reason",
        "full",
        "headline",
      ]) ||
      interval.id !== index + Number(value.omittedIntervals) ||
      !stages.slice(1).includes(String(interval.stage)) ||
      !uniqueIds(interval.participants, ids) ||
      !number(interval.startMs) ||
      !number(interval.endMs) ||
      interval.endMs < interval.startMs ||
      typeof interval.complete !== "boolean" ||
      !["stage-start", "dropout", "evidence-resumed"].includes(
        String(interval.reason),
      ) ||
      !window(interval.full, interval.participants, String(interval.stage)) ||
      !window(interval.headline, interval.participants, String(interval.stage))
    )
      return false;
  }
  return value.failures.every(
    (failure) =>
      object(failure) &&
      keys(failure, [
        "serverId",
        "stage",
        "atMs",
        "scope",
        "reason",
        "message",
      ]) &&
      ids.includes(String(failure.serverId)) &&
      stages.includes(String(failure.stage)) &&
      number(failure.atMs) &&
      ["throughput", "latency"].includes(String(failure.scope)) &&
      text(failure.reason) &&
      text(failure.message, 1024),
  );
}
