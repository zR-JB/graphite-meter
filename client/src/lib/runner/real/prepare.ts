import { abortableDelay, findCause, withinBudget } from "../abortable";
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
import { median } from "../measure";
import {
  blockedSelectionReason,
  browserProtocolMatchesTarget,
  candidates,
  classifyTransportDiscovery,
  connectionSelection,
  fetchViewOfOrigin,
  latencyPathNeeded,
  protocolFromNextHop,
  ROUTES,
  selectTarget,
  type AnyTarget,
  type ConnectionValidation,
  type ThroughputTarget,
} from "../paths";
import {
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
  H3_PROBE_ATTEMPTS,
  H3_PROBE_DEADLINE_MS,
} from "./budgets";
import { IdleKeepalive } from "./latencyChannel";
import { resourceProtocol } from "./resourceTiming";

/** The shared discovery request failed before either role could be checked. */
export class PreflightUnavailableError extends Error {}
/** A browser policy restriction whose message gives a known configuration remedy. */
export class BrowserOriginBlockedError extends Error {}

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
      `${credentials?.server.url ?? ""}${ROUTES.preflight}${ident}`,
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
    const timing = performance
      .getEntriesByName(response.url, "resource")
      .at(-1) as PerformanceResourceTiming | undefined;
    const { throughput, latency, uploadCheckpoint } = pf.capabilities;
    const secure = location.protocol === "https:";
    signal.throwIfAborted();
    return {
      ...classifyTransportDiscovery(
        throughput,
        latency,
        origin,
        secure,
        timing?.nextHopProtocol,
        location.origin,
      ),
      uploadCheckpoint,
      generation: pf.generation,
      engineVersion: pf.engineVersion,
      server: pf.server,
      fetchedAt: Date.now(),
      preflightMs,
    };
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
      const selection = connectionSelection(config, role);
      try {
        if (role === "throughput") {
          const path = await prepareRole(
            discovery,
            role,
            selection,
            signal,
            (target: ThroughputTarget, attempt) =>
              prepareThroughput(discovery, target, attempt, credentials),
          );
          result.validation.throughput = { selection, state: "verified", path };
        } else if (latencyPathNeeded(config)) {
          const { path, idle } = await prepareRole(
            discovery,
            role,
            selection,
            signal,
            (target: LatencyTarget, attempt) =>
              prepareLatency(discovery, target, attempt, credentials),
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
        const message =
          cause instanceof BrowserOriginBlockedError
            ? cause.message
            : "Connection check failed";
        result.validation[role] = {
          selection,
          state: "failed",
          path: null,
          message,
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

/** Automatic tries each candidate within its own budget; an explicit selection tries only itself. */
async function prepareRole<
  T extends AnyTarget,
  R extends { path: { requested: unknown } } | { requested: unknown },
>(
  discovery: TransportDiscovery,
  role: ConnectionRole,
  selection: string,
  signal: AbortSignal,
  attempt: (target: T, signal: AbortSignal) => Promise<R>,
): Promise<R> {
  const requested = selectTarget(discovery, role, selection) as T | null;
  if (!requested) {
    const restriction = blockedSelectionReason(discovery, role, selection);
    if (restriction) throw new BrowserOriginBlockedError(restriction);
    const unsupported = selectTarget(discovery, role, selection, true);
    throw new Error(
      unsupported
        ? `${unsupported.transport} is not supported by this client`
        : `${selection} ${role} target unavailable`,
    );
  }
  let failure: unknown;
  for (const target of selection === "auto"
    ? (candidates(discovery, role) as T[])
    : [requested]) {
    const budgetMs =
      H3_PROBE_DEADLINE_MS +
      (target.transport === "fetch-stream"
        ? 0
        : ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS);
    try {
      const result = await withinBudget(signal, budgetMs, (bounded) =>
        attempt(target, bounded),
      );
      if ("path" in result) result.path.requested = requested;
      else result.requested = requested;
      return result;
    } catch (cause) {
      signal.throwIfAborted();
      if (findCause(cause, ServerAuthenticationRequired)) throw cause;
      failure = cause;
    }
  }
  throw failure;
}

async function pathProbe(
  url: string,
  signal: AbortSignal,
  credentials?: ServerCredentials,
) {
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

/** HTTP/3 needs the browser to prove it used h3; WebTransport proves its own data path. */
async function prepareThroughput(
  discovery: TransportDiscovery,
  requested: ThroughputTarget,
  signal: AbortSignal,
  credentials?: ServerCredentials,
): Promise<VerifiedThroughputPath> {
  const wt = requested.transport !== "fetch-stream";
  const fetchTarget: FetchThroughputTarget = {
    ...(wt ? fetchViewOfOrigin(discovery, requested) : requested),
  };
  const deadline = new AbortController();
  const timeout =
    fetchTarget.protocol === "http3"
      ? setTimeout(() => deadline.abort(), H3_PROBE_DEADLINE_MS)
      : undefined;
  const probeSignal = AbortSignal.any([signal, deadline.signal]);
  let probe: VerifiedThroughputPath["probe"] | undefined;
  let browserProtocol: string | undefined;
  try {
    const attempts =
      !wt && fetchTarget.protocol === "http3" ? H3_PROBE_ATTEMPTS : 1;
    for (let attempt = 0; attempt < attempts; attempt++) {
      if (attempt)
        await abortableDelay(
          Math.min(250, 50 * 2 ** (attempt - 1)),
          probeSignal,
        );
      const url = `${fetchTarget.origin}${ROUTES.probe}?cb=${performance.now()}-${attempt}`;
      const answer = await pathProbe(url, probeSignal, credentials);
      probe = answer.probe;
      browserProtocol = await resourceProtocol(
        answer.response.url,
        probeSignal,
      );
      if (
        fetchTarget.protocol !== "http3" ||
        browserProtocolMatchesTarget(fetchTarget, browserProtocol)
      )
        break;
    }
    const proven = browserProtocolMatchesTarget(fetchTarget, browserProtocol);
    if (!probe || (!wt && !proven))
      throw new Error(`${fetchTarget.protocol} transport unavailable`);
    if (fetchTarget.protocol === "negotiated" || !proven)
      fetchTarget.protocol =
        protocolFromNextHop(browserProtocol) ?? "negotiated";
  } catch (cause) {
    signal.throwIfAborted();
    throw new Error(`${fetchTarget.protocol} transport unavailable`, { cause });
  } finally {
    clearTimeout(timeout);
  }
  if (wt) await verifyWtThroughput(requested, signal, credentials);
  signal.throwIfAborted();
  return {
    requested,
    target: wt ? requested : fetchTarget,
    fetch: fetchTarget,
    probe: probe!,
    browserProtocol,
    generation: discovery.generation,
    verifiedAt: Date.now(),
  };
}

/** Socket readiness, metadata and RTT collection are independent evidence, collected together. */
async function prepareLatency(
  discovery: TransportDiscovery,
  target: LatencyTarget,
  signal: AbortSignal,
  credentials?: ServerCredentials,
): Promise<{ path: VerifiedLatencyPath; idle: IdleKeepalive }> {
  const idle = new IdleKeepalive(target, performance.timeOrigin, credentials);
  const abort = () => idle.stop();
  signal.addEventListener("abort", abort, { once: true });
  try {
    const collecting = idle
      .verifyReady(signal)
      .then(() => idle.collectRtts(signal));
    const url = `${target.origin}${ROUTES.probe}?cb=${performance.now()}`;
    const [{ probe }, rtts] = await Promise.all([
      pathProbe(url, signal, credentials),
      collecting,
    ]);
    signal.throwIfAborted();
    const rttMs = rtts.length ? median(rtts) : null;
    const path = {
      requested: target,
      target,
      probe,
      rttMs,
      generation: discovery.generation,
      verifiedAt: Date.now(),
    };
    return { idle, path };
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
    let url = `${target.origin}${ROUTES.wtDownload}?bytes=${16 * 1024}${datagrams ? "&datagrams=1" : ""}`;
    const mint = socketMint(
      credentials,
      target.origin,
      ROUTES.wtDownload,
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
      const lane = datagrams
        ? { done: false, value: session.datagrams.readable }
        : await session.incomingUnidirectionalStreams.getReader().read();
      if (lane.done) throw new Error("no lane");
      const chunk = await (lane.value as ReadableStream<Uint8Array>)
        .getReader()
        .read();
      if (chunk.done || !chunk.value.byteLength)
        throw new Error("empty carrier");
    } finally {
      clearTimeout(deadline);
      signal.removeEventListener("abort", close);
      close();
    }
  } catch (cause) {
    signal.throwIfAborted();
    const message = established
      ? "webtransport session carried no bytes"
      : "webtransport session did not establish";
    throw new Error(message, { cause });
  }
}
