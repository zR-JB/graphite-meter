// Connection checks for one server: its discovery, and the proof that a chosen throughput or latency path carries
// data from this page, each within its budget. A failure carries the words its row shows, chosen where it happens,
// and whether time alone can clear it.
import {
  DEFAULT_STAGE_LIMIT_MS,
  preflight,
  probe as probeDocument,
  readJSON,
  safeDetail,
  socketTicket,
  type CatalogEntry,
  type Probe,
} from "../../protocol/documents";
import { ROUTES } from "../../protocol/routes";
import {
  SignInRequired,
  type Discovery,
  type LatencyPath,
  type LatencyTarget,
  type OpenProbeChannel,
  type ProbeChannel,
  type Role,
  type ServerAuth,
  type SessionTarget,
  type ThroughputPath,
  type ThroughputTarget,
} from "../types";
import type { PageConfig } from "./page";
import {
  allowsOrigin,
  candidates,
  classify,
  fetchView,
  originPolicy,
  PROTOCOL_LABEL,
  protocolFromNextHop,
  resolve,
  selectionText,
  unresolvedReason,
  type Selection,
} from "./paths";

export type Retry = "retryable" | "needs-user" | "impossible";

/** A check that failed for a reason the user can read; `retry` says whether time alone can clear it. */
export class CheckError extends Error {
  constructor(
    message: string,
    readonly retry: Retry,
    options?: ErrorOptions,
  ) {
    super(message, options);
  }
}

/** A failed check as its server's row shows it. */
export interface Failure {
  message: string;
  retry: Retry;
  signIn: boolean;
}

/** What a check needs of its server and this page. */
export interface CheckContext {
  entry: CatalogEntry;
  auth: ServerAuth;
  page: PageConfig;
  openProbeChannel: OpenProbeChannel;
  now(): number;
}

/** A path that has not answered by now is not going to. */
const ESTABLISH_MS = 3_000;
/** Lets an inner bound (a channel's establishment, the HTTP/3 bootstrap) report before its owner gives up. */
const ESTABLISH_MARGIN_MS = 500;
// HTTP/3 bootstrap is bounded by attempts and by wall time together, so a path that never answers cannot hold a check.
const H3_ATTEMPTS = 8;
const H3_DEADLINE_MS = 2_000;
/** Body completion can precede delivery of the browser's Resource Timing entry. */
const TIMING_DELIVERY_MS = 250;
const PREPARATION_INTERVAL_MS = 120;
const PREPARATION_RTTS = 5;
const COLLECT_MS = 1_500;
const SESSION_CHECK_BYTES = 16 * 1024;

const NETWORK_FAILURE =
  /failed to fetch|fetch failed|network(?:error| request failed)|load failed|connection (?:refused|reset|lost)/i;

/** The error and its causes, outermost first; engines word one network failure differently. */
function* causes(error: unknown): Generator<Error> {
  for (let depth = 0; error instanceof Error && depth < 16; error = error.cause, depth++) yield error;
}
const isNetworkFailure = (error: unknown) =>
  [...causes(error)].some(({ name, message }) => name === "NetworkError" || NETWORK_FAILURE.test(message));
const signsIn = (error: unknown) => [...causes(error)].some((cause) => cause instanceof SignInRequired);

/** How a failed check reads on its server's row, `name` being the server's displayed name. */
export function failureOf(error: unknown, name: string): Failure {
  if (signsIn(error)) return { message: `Sign in to ${name}`, retry: "needs-user", signIn: true };
  if (error instanceof CheckError) return { message: error.message, retry: error.retry, signIn: false };
  const timedOut = error instanceof DOMException && error.name === "TimeoutError";
  return {
    message: timedOut ? "Connection check timed out" : "Connection check failed",
    retry: "retryable",
    signIn: false,
  };
}

export function delay(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) return reject(signal.reason);
    const abort = () => {
      clearTimeout(timer);
      reject(signal.reason);
    };
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", abort);
      resolve();
    }, ms);
    signal.addEventListener("abort", abort, { once: true });
  });
}

/** Runs `run` within `ms` of the owner's lifetime, even if it ignores its signal; its own deadline reads "Connection
    check timed out". */
export async function withinBudget<T>(owner: AbortSignal, ms: number, run: (signal: AbortSignal) => Promise<T>) {
  owner.throwIfAborted();
  const deadline = AbortSignal.timeout(ms);
  const signal = AbortSignal.any([owner, deadline]);
  try {
    return await new Promise<T>((resolve, reject) => {
      const abort = () => reject(signal.reason);
      signal.addEventListener("abort", abort, { once: true });
      run(signal)
        .then(resolve, reject)
        .finally(() => signal.removeEventListener("abort", abort));
    });
  } catch (error) {
    if (!owner.aborted && deadline.aborted)
      throw new CheckError("Connection check timed out", "retryable", { cause: error });
    throw error;
  }
}

let requests = 0;

/** The browser's next-hop protocol for one unique URL, watched from before the request so that neither a full
    Resource Timing buffer nor an older request's late entry can hide or replace it. */
function watchProtocol(url: string) {
  let protocol: string | null | undefined;
  let delivered: (() => void) | null = null;
  const take = (entries: PerformanceEntryList) => {
    const entry = entries.at(-1) as PerformanceResourceTiming | undefined;
    if (!entry) return;
    protocol = entry.nextHopProtocol || null;
    delivered?.();
  };
  let observer: PerformanceObserver | null = null;
  try {
    observer = new PerformanceObserver((list) => take(list.getEntriesByName(url)));
    observer.observe({ type: "resource" });
  } catch {
    observer = null;
  }
  return {
    async read(signal: AbortSignal): Promise<string | null> {
      if (protocol === undefined) take(performance.getEntriesByName(url, "resource"));
      if (protocol === undefined && observer)
        await new Promise<void>((resolve) => {
          const done = () => {
            clearTimeout(timer);
            signal.removeEventListener("abort", done);
            delivered = null;
            resolve();
          };
          const timer = setTimeout(done, TIMING_DELIVERY_MS);
          signal.addEventListener("abort", done, { once: true });
          delivered = done;
        });
      signal.throwIfAborted();
      return protocol ?? null;
    },
    stop: () => observer?.disconnect(),
  };
}

const release = (response: Response) => response.body?.cancel().catch(() => {});

async function decoded<T>(response: Response, decode: (value: unknown) => T, message: string): Promise<T> {
  try {
    return decode(await readJSON(response));
  } catch (cause) {
    throw new CheckError(message, "retryable", { cause });
  }
}

/** A plain refusal's first line says why, such as a proxy that names no client; any other body is released unread. */
async function refusal(response: Response): Promise<string> {
  const body = response.headers.get("Content-Type")?.startsWith("text/plain") ? response.body : null;
  if (!body) {
    await release(response);
    return "";
  }
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let text = "";
  try {
    while (text.length < 4096 && !text.includes("\n")) {
      const { done, value } = await reader.read();
      if (done) break;
      text += decoder.decode(value, { stream: true });
    }
  } catch {
    return "";
  } finally {
    reader.cancel().catch(() => {});
  }
  const line = safeDetail(text.split("\n", 1)[0]!.trim(), 200);
  return line && `: ${line}`;
}

/** A failure after the request left: the page login gets its one expiry check, then the failure is worded. */
async function settled(error: unknown, auth: ServerAuth, signal: AbortSignal, unreachable: string): Promise<never> {
  signal.throwIfAborted();
  if (auth.kind === "session" && !signsIn(error)) await auth.ended(signal);
  if (error instanceof CheckError || signsIn(error)) throw error;
  throw new CheckError(isNetworkFailure(error) ? unreachable : "Connection check failed", "retryable", {
    cause: error,
  });
}

/** One GET /preflight, decoded, held to the catalogue entry's origins and classified for this page; no probe, socket
    or other side effect. A browser policy refuses it before any request. */
export async function discover(context: CheckContext, signal: AbortSignal): Promise<Discovery> {
  const { entry, auth, page, now } = context;
  signal.throwIfAborted();
  const blocked = originPolicy(entry.url, page);
  if (blocked) throw new CheckError(blocked, "impossible");
  const version = encodeURIComponent(BUILD.clientVersion);
  const url = `${entry.url}${ROUTES.preflight}?client=web&client_version=${version}&cb=${++requests}`;
  const timing = watchProtocol(url);
  const startedAt = now();
  try {
    const response = await auth.fetch(url, { cache: "no-store", signal });
    if (!response.ok) {
      await release(response);
      throw new CheckError(`Connection check failed (HTTP ${response.status})`, "retryable");
    }
    const document = await decoded(response, preflight, "Connection check failed (unreadable discovery)");
    const requestMs = now() - startedAt;
    const { capabilities } = document;
    if (
      [...capabilities.throughput, ...capabilities.latency].some(
        (t) => t.baseUrl !== "." && !allowsOrigin(entry, t.baseUrl),
      )
    )
      throw new CheckError(`${entry.name} advertised an origin outside its catalog entry`, "retryable");
    const protocol = await timing.read(signal);
    return {
      generation: document.generation,
      engineVersion: document.engineVersion,
      server: document.server,
      uploadCheckpoint: capabilities.uploadCheckpoint === true,
      maxStageMs: capabilities.maxStageMs ?? DEFAULT_STAGE_LIMIT_MS,
      // Redirects are refused, so the answer came from the catalogue's origin, which "." names.
      origin: entry.url,
      protocol,
      readAt: now(),
      requestMs,
      ...classify(capabilities, entry.url, page),
    };
  } catch (error) {
    // A protected HTTPS server refuses another origin without CORS headers, which reads as a network failure.
    const hint =
      entry.url.startsWith("https:") && !page.secure ? ". If it requires sign-in, open this interface over HTTPS." : "";
    return settled(error, auth, signal, `Server could not be reached${hint}`);
  } finally {
    timing.stop();
  }
}

async function probeOrigin(origin: string, auth: ServerAuth, signal: AbortSignal) {
  const url = `${origin}${ROUTES.probe}?cb=${++requests}`;
  const timing = watchProtocol(url);
  try {
    const response = await auth.fetch(url, { cache: "no-store", signal });
    if (!response.ok)
      throw new CheckError(`Probe answered HTTP ${response.status}${await refusal(response)}`, "retryable");
    const probe: Probe = await decoded(response, probeDocument, "Unexpected probe answer");
    return { probe, protocol: await timing.read(signal) };
  } catch (error) {
    return settled(error, auth, signal, `${origin} could not be reached`);
  } finally {
    timing.stop();
  }
}

/** The targets a check tries: Automatic's candidates in rank order, or only what an explicit choice resolves to.
    Nothing to try fails before any request. */
function targetsFor<R extends Role>(context: CheckContext, discovery: Discovery, role: R, selection: Selection) {
  const requested = resolve(discovery, role, selection, context.page);
  if (!requested) {
    const { message, retry } = unresolvedReason(discovery, role, selection, context.page);
    throw new CheckError(message, retry);
  }
  return selection.kind === "auto" ? candidates(discovery, role, context.page) : [requested];
}

/** The first target that verifies, each within its own budget. Automatic stops at a sign-in refusal rather than
    probing another endpoint; when all fail, the preferred target's failure says the most. */
async function firstVerified<T extends ThroughputTarget | LatencyTarget, P>(
  targets: readonly T[],
  signal: AbortSignal,
  verify: (target: T, signal: AbortSignal) => Promise<P>,
): Promise<P> {
  let failure: unknown;
  let retryable = false;
  for (const target of targets) {
    // Sockets add their establishment to the HTTP/3 bootstrap's wall time; each inner bound keeps its margin.
    const sockets = target.transport === "fetch-stream" ? 0 : ESTABLISH_MS + ESTABLISH_MARGIN_MS;
    const budgetMs = H3_DEADLINE_MS + ESTABLISH_MARGIN_MS + sockets;
    try {
      return await withinBudget(signal, budgetMs, (bounded) => verify(target, bounded));
    } catch (error) {
      signal.throwIfAborted();
      if (signsIn(error)) throw error;
      failure ??= error;
      retryable ||= !(error instanceof CheckError) || error.retry === "retryable";
    }
  }
  // While any target may still answer, time can clear the role, whatever the preferred one said.
  if (retryable && failure instanceof CheckError && failure.retry !== "retryable")
    throw new CheckError(failure.message, "retryable", { cause: failure });
  throw failure;
}

/** Proves the throughput path `selection` names: the HTTP view of its origin answers a probe over the protocol it
    claims, and a WebTransport session delivers bytes. */
export async function checkThroughput(
  context: CheckContext,
  discovery: Discovery,
  selection: Selection,
  signal: AbortSignal,
): Promise<ThroughputPath> {
  const targets = targetsFor(context, discovery, "throughput", selection);
  const verifiedFor = { selection: selectionText(selection), head: targets[0]!.id };
  return firstVerified(targets, signal, async (target, bounded) => ({
    ...(await verifyThroughput(context, discovery, target, bounded)),
    generation: discovery.generation,
    verifiedAt: context.now(),
    verifiedFor,
  }));
}

async function verifyThroughput(
  context: CheckContext,
  discovery: Discovery,
  target: ThroughputTarget,
  signal: AbortSignal,
) {
  const session = target.transport !== "fetch-stream";
  const view = session ? fetchView(discovery, target) : { ...target };
  // The browser adopts HTTP/3 only after an Alt-Svc answer, so an HTTP/3 target is probed until it is seen in use.
  const bounded = view.protocol === "http3" ? AbortSignal.any([signal, AbortSignal.timeout(H3_DEADLINE_MS)]) : signal;
  let answer: { probe: Probe; protocol: string | null } | null = null;
  try {
    for (let attempt = 0; attempt < (session || view.protocol !== "http3" ? 1 : H3_ATTEMPTS); attempt++) {
      if (attempt) await delay(Math.min(250, 50 * 2 ** (attempt - 1)), bounded);
      answer = await probeOrigin(view.origin, context.auth, bounded);
      if (protocolFromNextHop(answer.protocol) === "http3") break;
    }
  } catch (error) {
    // The bootstrap ran out of time: the last answer says what the browser used.
    if (signal.aborted || !bounded.aborted || !answer) throw error;
  }
  const observed = protocolFromNextHop(answer!.protocol);
  const proven = view.protocol === "negotiated" || observed === view.protocol;
  // WebTransport proves its own data path; only its HTTP view's protocol is learnt here.
  if (!session && !proven)
    throw new CheckError(
      observed
        ? `The browser used ${PROTOCOL_LABEL[observed]}, not ${PROTOCOL_LABEL[view.protocol]}`
        : `The browser did not confirm ${PROTOCOL_LABEL[view.protocol]}`,
      "retryable",
    );
  if (!proven || view.protocol === "negotiated") view.protocol = observed ?? "negotiated";
  if (session) await verifySession(context.auth, target, signal);
  return { target: session ? target : view, fetch: view, probe: answer!.probe, browserProtocol: answer!.protocol };
}

/** Opening a session is not enough: the selected transfer must deliver a byte. */
async function verifySession(auth: ServerAuth, target: SessionTarget, signal: AbortSignal): Promise<void> {
  const datagrams = target.transport === "webtransport-datagram";
  let url = `${target.origin}${ROUTES.wtDownload}?bytes=${SESSION_CHECK_BYTES}${datagrams ? "&datagrams=1" : ""}`;
  const ticket = auth.ticket(target.origin, ROUTES.wtDownload);
  if (ticket) {
    const response = await auth.fetch(ticket.url, { method: "POST", cache: "no-store", signal });
    if (!response.ok) {
      await release(response);
      throw new CheckError(`WebTransport ticket refused (HTTP ${response.status})`, "retryable");
    }
    const { token } = await decoded(response, socketTicket, "Unexpected WebTransport ticket");
    // Authentication off answers an empty ticket, which the dial must not carry.
    if (token) url += `&token=${encodeURIComponent(token)}`;
  }
  signal.throwIfAborted();
  let session: WebTransport | null = null;
  const close = () => {
    try {
      session?.close();
    } catch {}
  };
  const deadline = setTimeout(close, ESTABLISH_MS);
  signal.addEventListener("abort", close, { once: true });
  let established = false;
  try {
    session = new WebTransport(url);
    session.closed.catch(() => {});
    await session.ready;
    established = true;
    const carrier: ReadableStream<Uint8Array> | undefined = datagrams
      ? session.datagrams.readable
      : (await session.incomingUnidirectionalStreams.getReader().read()).value;
    const chunk = await carrier?.getReader().read();
    if (!chunk?.value?.byteLength) throw new Error("the carrier delivered no bytes");
  } catch (cause) {
    signal.throwIfAborted();
    const message = established ? "WebTransport session carried no bytes" : "WebTransport session did not establish";
    throw new CheckError(message, "retryable", { cause });
  } finally {
    clearTimeout(deadline);
    signal.removeEventListener("abort", close);
    close();
  }
}

/** Proves the latency path `selection` names: its origin answers a probe while its bus answers pings, both at once. */
export async function checkLatency(
  context: CheckContext,
  discovery: Discovery,
  selection: Selection,
  signal: AbortSignal,
): Promise<LatencyPath> {
  const targets = targetsFor(context, discovery, "latency", selection);
  const verifiedFor = { selection: selectionText(selection), head: targets[0]!.id };
  return firstVerified(targets, signal, async (target, bounded) => {
    // Either failing ends the other at once.
    const both = new AbortController();
    const linked = AbortSignal.any([bounded, both.signal]);
    try {
      const [{ probe }, rttMs] = await Promise.all([
        probeOrigin(target.origin, context.auth, linked),
        collectRtt(context, target, linked),
      ]);
      return { target, probe, rttMs, generation: discovery.generation, verifiedAt: context.now(), verifiedFor };
    } finally {
      both.abort();
    }
  });
}

/** Opens the bus at the preparation cadence: it must answer within 3.5 s, then up to five replies are collected for
    at most 1.5 s; their median, or null without one. */
function collectRtt(context: CheckContext, target: LatencyTarget, signal: AbortSignal): Promise<number | null> {
  if (signal.aborted) return Promise.reject(signal.reason);
  const { promise, resolve, reject } = Promise.withResolvers<number | null>();
  const rtts: number[] = [];
  let channel: ProbeChannel | null = null;
  let collecting = false;
  let done = false;
  const finish = (error?: unknown) => {
    if (done) return;
    done = true;
    clearTimeout(timer);
    signal.removeEventListener("abort", abort);
    channel?.dispose();
    if (error !== undefined) return reject(error);
    const sorted = rtts.toSorted((a, b) => a - b);
    const middle = sorted.length >> 1;
    resolve(!sorted.length ? null : sorted.length % 2 ? sorted[middle]! : (sorted[middle - 1]! + sorted[middle]!) / 2);
  };
  const abort = () => finish(signal.reason);
  let timer = setTimeout(
    () => finish(new CheckError("Latency channel did not become ready", "retryable")),
    ESTABLISH_MS + ESTABLISH_MARGIN_MS,
  );
  signal.addEventListener("abort", abort, { once: true });
  const { entry, auth } = context;
  try {
    channel = context.openProbeChannel(
      { target, auth, intervalMs: PREPARATION_INTERVAL_MS, maxInFlight: 2 },
      {
        ready() {
          if (done || collecting) return;
          collecting = true;
          clearTimeout(timer);
          timer = setTimeout(finish, COLLECT_MS);
        },
        outcomes(batch) {
          for (const outcome of batch) if (!outcome.timedOut) rtts.push(outcome.rttMs);
          if (rtts.length >= PREPARATION_RTTS) finish();
        },
        interrupted() {},
        stalled() {},
        resumed() {},
        signInRequired: () => finish(new SignInRequired({ id: entry.id, url: entry.url, name: entry.name })),
      },
    );
    if (done) channel.dispose();
    else channel.measure();
  } catch (error) {
    finish(new CheckError("Latency channel did not open", "retryable", { cause: error }));
  }
  return promise;
}
