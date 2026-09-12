import { withinBudget, abortableDelay } from "../abortable";
import type {
  ConnectionRole,
  RunnerConfig,
  TransportDiscovery,
  VerifiedLatencyPath,
  VerifiedThroughputPath,
} from "../contract";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../../api/endpoints";
import type { Probe } from "../../api/probe";
import {
  readJSONResponse,
  parsePreflight,
  parseProbe,
  parseWtToken,
} from "../../api/decode";
import {
  measurementFetch,
  ServerAuthenticationRequired,
  classifyServerAuthentication,
  socketMint,
  type ServerCredentials,
} from "../../servers/credentials";
import {
  browserOriginRestriction,
  validateServerDiscovery,
} from "../../servers/catalog";
import { BUILD } from "../../buildenv";
import {
  latencyPathNeeded,
  type ConnectionValidation,
} from "../connectionModel";
import { median } from "../stats";
import {
  automaticThroughputTargets,
  automaticLatencyTargets,
  browserProtocolMatchesTarget,
  blockedSelectionReason,
  classifyTransportDiscovery,
  fetchViewOfOrigin,
  protocolFromNextHop,
  selectLatencyTarget,
  selectThroughputTarget,
} from "./backendPure";
import {
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
  H3_PROBE_ATTEMPTS,
  H3_PROBE_DEADLINE_MS,
} from "./budgets";
import { IdleKeepalive } from "./latencyChannel";
import { resourceProtocol } from "./resourceTiming";
import {
  PreflightUnavailableError,
  BrowserOriginBlockedError,
  TransportUnavailableError,
} from "./transportError";
import { transportRunnable } from "./transports";

export interface ConnectionPreparation {
  discovery: TransportDiscovery;
  validation: ConnectionValidation;
  /** Undefined keeps the committed monitor; null clears it. A new monitor is adopted only after commit. */
  idle?: Pick<IdleKeepalive, "start" | "stop" | "onEvent"> | null;
  failure?: unknown;
}

/** Bounded discovery has no measurement sockets or path-validation side effects. */
export async function discoverServer(
  signal: AbortSignal,
  credentials?: ServerCredentials,
): Promise<TransportDiscovery> {
  signal.throwIfAborted();
  const restriction = browserOriginRestriction(
    credentials?.server.url ?? location.origin,
    location.origin,
  );
  if (restriction) throw new BrowserOriginBlockedError(restriction);
  try {
    const ident = `?client=web&client_version=${encodeURIComponent(BUILD.clientVersion)}`;
    const startedAt = performance.now();
    const response = await measurementFetch(
      credentials,
      `${credentials?.server.url ?? ""}/preflight${ident}`,
      {
        cache: "no-store",
        signal,
      },
    );
    if (!response.ok)
      throw new Error(`preflight returned HTTP ${response.status}`);
    const data = await readJSONResponse(response);
    const preflightMs = performance.now() - startedAt;
    const pf = parsePreflight(data);
    if (credentials) validateServerDiscovery(credentials.server, pf);
    const origin = new URL(response.url, location.href).origin;
    const protocol = (
      performance.getEntriesByName(response.url, "resource").at(-1) as
        PerformanceResourceTiming | undefined
    )?.nextHopProtocol;
    const discovery: TransportDiscovery = {
      ...classifyTransportDiscovery(
        pf.capabilities.throughput,
        pf.capabilities.latency,
        origin,
        location.protocol === "https:",
        protocol,
        location.origin,
      ),
      uploadCheckpoint: pf.capabilities.uploadCheckpoint,
      generation: pf.generation,
      engineVersion: pf.engineVersion,
      server: pf.server,
      fetchedAt: Date.now(),
      preflightMs,
    };
    signal.throwIfAborted();
    return discovery;
  } catch (cause) {
    signal.throwIfAborted();
    await classifyServerAuthentication(credentials, signal);
    throw new PreflightUnavailableError("preflight unavailable", { cause });
  }
}

/** Preparation owns provisional sockets. Only its caller can commit the returned evidence and monitor. */
export async function prepareConnections(
  config: RunnerConfig,
  previous: ConnectionValidation,
  roles: ConnectionRole[],
  signal: AbortSignal,
  credentials?: ServerCredentials,
  knownDiscovery?: TransportDiscovery,
): Promise<ConnectionPreparation> {
  const discovery =
    knownDiscovery ?? (await discoverServer(signal, credentials));
  signal.throwIfAborted();
  const result: ConnectionPreparation = {
    discovery,
    validation: { ...previous },
  };
  try {
    for (const role of roles) {
      const selection =
        role === "throughput"
          ? config.transports.throughputTarget
          : config.transports.latencyTarget;
      try {
        if (role === "throughput") {
          const path = await prepareThroughput(
            discovery,
            selection,
            signal,
            credentials,
          );
          result.validation.throughput = { selection, state: "verified", path };
        } else if (latencyPathNeeded(config)) {
          const { path, idle } = await prepareLatency(
            discovery,
            selection,
            signal,
            credentials,
          );
          result.idle = idle;
          result.validation.latency = { selection, state: "verified", path };
        } else {
          result.idle = null;
          result.validation.latency = { selection, state: "stale", path: null };
        }
      } catch (cause) {
        signal.throwIfAborted();
        result.failure ??= cause;
        result.validation[role] = {
          selection,
          state: "failed",
          path: null,
          message:
            cause instanceof BrowserOriginBlockedError
              ? cause.message
              : "Connection check failed",
        };
        if (role === "latency") result.idle = null;
      }
    }
    signal.throwIfAborted();
    return result;
  } catch (cause) {
    result.idle?.stop();
    throw cause;
  }
}

async function pathProbe(
  url: string,
  signal: AbortSignal,
  credentials?: ServerCredentials,
): Promise<{ probe: Probe; response: Response }> {
  try {
    const response = await measurementFetch(credentials, url, {
      cache: "no-store",
      signal,
    });
    if (!response.ok) throw new Error(`probe returned HTTP ${response.status}`);
    return { response, probe: parseProbe(await readJSONResponse(response)) };
  } catch (cause) {
    signal.throwIfAborted();
    await classifyServerAuthentication(credentials, signal);
    throw cause;
  }
}

async function prepareThroughput(
  discovery: TransportDiscovery,
  selection: string,
  signal: AbortSignal,
  credentials?: ServerCredentials,
): Promise<VerifiedThroughputPath> {
  const requested = selectThroughputTarget(discovery, selection, true);
  const restriction = blockedSelectionReason(
    discovery,
    "throughput",
    selection,
  );
  if (!requested && restriction)
    throw new BrowserOriginBlockedError(restriction);
  if (!requested)
    throw new TransportUnavailableError(`${selection} target unavailable`, {
      role: "throughput",
    });
  if (!transportRunnable(requested.transport))
    throw new TransportUnavailableError(
      `${requested.transport} is not supported by this client`,
      { role: "throughput" },
    );
  const candidates =
    selection === "auto"
      ? automaticThroughputTargets(discovery, transportRunnable("webtransport"))
      : [requested];
  let failure: unknown;
  for (const target of candidates) {
    try {
      const path = await withinBudget(
        signal,
        target.transport === "fetch-stream"
          ? H3_PROBE_DEADLINE_MS
          : H3_PROBE_DEADLINE_MS + ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS,
        (attemptSignal) =>
          prepareThroughputTarget(
            discovery,
            target,
            attemptSignal,
            credentials,
          ),
      );
      return { ...path, requested };
    } catch (cause) {
      signal.throwIfAborted();
      if (authenticationFailure(cause)) throw cause;
      failure = cause;
    }
  }
  throw failure;
}

async function prepareThroughputTarget(
  discovery: TransportDiscovery,
  requested: FetchThroughputTarget | WebTransportThroughputTarget,
  signal: AbortSignal,
  credentials?: ServerCredentials,
): Promise<VerifiedThroughputPath> {
  const fetchTarget: FetchThroughputTarget = {
    ...(requested.transport === "fetch-stream"
      ? requested
      : fetchViewOfOrigin(discovery, requested)),
  };
  const deadline = new AbortController();
  const timeout =
    fetchTarget.protocol === "http3"
      ? setTimeout(() => deadline.abort(), H3_PROBE_DEADLINE_MS)
      : undefined;
  const probeSignal = AbortSignal.any([signal, deadline.signal]);
  let probe: Probe | undefined;
  let browserProtocol: string | undefined;
  try {
    const attempts =
      requested.transport === "fetch-stream" && fetchTarget.protocol === "http3"
        ? H3_PROBE_ATTEMPTS
        : 1;
    for (let attempt = 0; attempt < attempts; attempt++) {
      const response = await pathProbe(
        `${fetchTarget.origin}${fetchTarget.routes.probe}?cb=${performance.now()}-${attempt}`,
        probeSignal,
        credentials,
      );
      probe = response.probe;
      browserProtocol = await resourceProtocol(
        response.response.url,
        probeSignal,
      );
      if (
        fetchTarget.protocol !== "http3" ||
        browserProtocolMatchesTarget(fetchTarget, browserProtocol)
      )
        break;
      if (attempt + 1 < attempts)
        await abortableDelay(Math.min(250, 50 * 2 ** attempt), probeSignal);
    }
    const protocolProven = browserProtocolMatchesTarget(
      fetchTarget,
      browserProtocol,
    );
    // WebTransport proves its own data path below; its HTTP probe is control traffic.
    if (!probe || (requested.transport === "fetch-stream" && !protocolProven))
      throw new Error(`${fetchTarget.protocol} transport unavailable`);
    if (fetchTarget.protocol === "negotiated" || !protocolProven)
      fetchTarget.protocol =
        protocolFromNextHop(browserProtocol) ?? "negotiated";
  } catch (cause) {
    signal.throwIfAborted();
    throw new TransportUnavailableError(
      `${fetchTarget.protocol} transport unavailable`,
      { cause, role: "throughput" },
    );
  } finally {
    if (timeout !== undefined) clearTimeout(timeout);
  }
  const target =
    requested.transport === "fetch-stream" ? fetchTarget : requested;
  if (requested.transport !== "fetch-stream")
    await verifyWtThroughput(requested, signal, credentials);
  signal.throwIfAborted();
  return {
    requested,
    target,
    fetch: fetchTarget,
    probe: probe!,
    browserProtocol,
    generation: discovery.generation,
    verifiedAt: Date.now(),
  };
}

async function prepareLatency(
  discovery: TransportDiscovery,
  selection: string,
  signal: AbortSignal,
  credentials?: ServerCredentials,
): Promise<{ path: VerifiedLatencyPath; idle: IdleKeepalive }> {
  const requested = selectLatencyTarget(
    discovery,
    selection,
    transportRunnable("webtransport"),
  );
  const restriction = blockedSelectionReason(discovery, "latency", selection);
  if (!requested && restriction)
    throw new BrowserOriginBlockedError(restriction);
  if (!requested)
    throw new TransportUnavailableError(
      `${selection} latency target unavailable`,
      { role: "latency" },
    );
  const candidates =
    selection === "auto"
      ? automaticLatencyTargets(discovery, transportRunnable("webtransport"))
      : [requested];
  let failure: unknown;
  for (const target of candidates) {
    try {
      const result = await withinBudget(
        signal,
        ESTABLISH_BUDGET_MS + H3_PROBE_DEADLINE_MS + ESTABLISH_MARGIN_MS,
        (attemptSignal) =>
          prepareLatencyTarget(discovery, target, attemptSignal, credentials),
      );
      return { ...result, path: { ...result.path, requested } };
    } catch (cause) {
      signal.throwIfAborted();
      if (authenticationFailure(cause)) throw cause;
      failure = cause;
    }
  }
  throw failure;
}

async function prepareLatencyTarget(
  discovery: TransportDiscovery,
  target: LatencyTarget,
  signal: AbortSignal,
  credentials?: ServerCredentials,
): Promise<{ path: VerifiedLatencyPath; idle: IdleKeepalive }> {
  const idle = new IdleKeepalive(target, performance.timeOrigin, credentials);
  const abort = () => idle.stop();
  signal.addEventListener("abort", abort, { once: true });
  try {
    await idle.verifyReady(signal);
    const { probe } = await pathProbe(
      `${target.origin}${target.routes.probe}?cb=${performance.now()}`,
      signal,
      credentials,
    );
    const rtts = await idle.collectRtts(signal);
    signal.throwIfAborted();
    return {
      idle,
      path: {
        requested: target,
        target,
        probe,
        rttMs: rtts.length ? median(rtts) : null,
        generation: discovery.generation,
        verifiedAt: Date.now(),
      },
    };
  } catch (cause) {
    idle.stop();
    throw cause;
  } finally {
    signal.removeEventListener("abort", abort);
  }
}

/** Opening a session is insufficient: the selected transfer must deliver a byte. */
async function verifyWtThroughput(
  target: WebTransportThroughputTarget,
  signal: AbortSignal,
  credentials?: ServerCredentials,
): Promise<void> {
  let established = false;
  try {
    const datagrams = target.transport === "webtransport-datagram";
    let url = `${target.origin}${target.routes.wtDownload}?bytes=${16 * 1024}${datagrams ? "&datagrams=1" : ""}`;
    const mint = socketMint(
      credentials,
      target.origin,
      target.routes.wtDownload,
      "wt",
    );
    if (mint) {
      const minted = await measurementFetch(credentials, mint.url, {
        method: "POST",
        cache: "no-store",
        signal,
      });
      if (!minted.ok)
        throw new Error(`webtransport token mint refused (${minted.status})`);
      url += `&token=${encodeURIComponent(parseWtToken(await readJSONResponse(minted)).token)}`;
    }
    signal.throwIfAborted();
    const session = new WebTransport(url);
    void session.closed.catch(() => {});
    const close = () => session.close();
    signal.addEventListener("abort", close, { once: true });
    const deadline = setTimeout(close, ESTABLISH_BUDGET_MS);
    try {
      await session.ready;
      established = true;
      let stream: ReadableStream<Uint8Array>;
      if (datagrams) stream = session.datagrams.readable;
      else {
        const lane = await session.incomingUnidirectionalStreams
          .getReader()
          .read();
        if (lane.done) throw new Error("no lane");
        stream = lane.value as ReadableStream<Uint8Array>;
      }
      const chunk = await stream.getReader().read();
      if (chunk.done || !chunk.value.byteLength)
        throw new Error("empty carrier");
    } finally {
      clearTimeout(deadline);
      signal.removeEventListener("abort", close);
      close();
    }
  } catch (cause) {
    signal.throwIfAborted();
    throw new TransportUnavailableError(
      established
        ? "webtransport session carried no bytes"
        : "webtransport session did not establish",
      { cause, role: "throughput" },
    );
  }
}

function authenticationFailure(cause: unknown): boolean {
  const seen = new Set<Error>();
  while (cause instanceof Error && !seen.has(cause)) {
    seen.add(cause);
    if (cause instanceof ServerAuthenticationRequired) return true;
    cause = cause.cause;
  }
  return false;
}
