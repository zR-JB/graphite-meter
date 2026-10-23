// Connection checks for one server: discovery and proof that a chosen path carries data from this page.
import {
  preflight,
  probe as probeDocument,
  readJSON,
  release,
  safeDetail,
  socketTicket,
  type CatalogEntry,
  type Probe,
} from "../../protocol/documents";
import { ROUTES } from "../../protocol/routes";
import { ESTABLISH_MS } from "../lanes/endings";
import { midpointMedian } from "../measure/latency";
import { abortable, delay } from "../run/clock";
import {
  causes,
  identityOf,
  isNetworkFailure,
  SignInRequired,
  type Discovery,
  type LatencyPath,
  type LatencyTarget,
  type OpenProbeChannel,
  type ProbeChannel,
  type ProbeChannelEvents,
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
import { handshakes, THROTTLED } from "./webtransport";

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

export interface Failure {
  message: string;
  retry: Retry;
  signIn: boolean;
}

export interface CheckContext {
  entry: CatalogEntry;
  auth: ServerAuth;
  page: PageConfig;
  openProbeChannel: OpenProbeChannel;
  now(): number;
}

/** Lets an inner bound (a channel's establishment, the HTTP/3 bootstrap) report before its owner gives up. */
const ESTABLISH_MARGIN_MS = 500;
// HTTP/3 bootstrap is bounded by attempts and by wall time together, so a path that never answers cannot hold a check.
const H3_ATTEMPTS = 8;
const H3_DEADLINE_MS = 2_000;
/** Body completion can precede the browser's Resource Timing entry; a loaded browser delivers it late. */
const TIMING_DELIVERY_MS = 1_000;
const PREPARATION_INTERVAL_MS = 120;
const PREPARATION_RTTS = 5;
const COLLECT_MS = 1_500;
const SESSION_CHECK_BYTES = 16 * 1024;

const signsIn = (error: unknown) =>
  [...causes(error)].some((cause) => cause instanceof SignInRequired);

export const CHECK_FAILED = "Could not check the connection";

/** How a failed check reads on its server's row, `name` being the server's displayed name. */
export function failureOf(error: unknown, name: string): Failure {
  if (signsIn(error)) return { message: `Sign in to ${name}`, retry: "needs-user", signIn: true };
  if (error instanceof CheckError)
    return { message: error.message, retry: error.retry, signIn: false };
  const timedOut = error instanceof DOMException && error.name === "TimeoutError";
  return {
    message: timedOut ? "Connection check timed out" : CHECK_FAILED,
    retry: "retryable",
    signIn: false,
  };
}

/** Runs `run` within `ms` of the owner's lifetime, even if it ignores its signal. */
export async function withinBudget<T>(
  owner: AbortSignal,
  ms: number,
  run: (signal: AbortSignal) => Promise<T>,
) {
  owner.throwIfAborted();
  const deadline = AbortSignal.timeout(ms);
  const signal = AbortSignal.any([owner, deadline]);
  try {
    return await abortable(run(signal), signal);
  } catch (error) {
    if (!owner.aborted && deadline.aborted)
      throw new CheckError("Connection check timed out", "retryable", { cause: error });
    throw error;
  }
}

let requests = 0;

/** The browser's next-hop protocol for one unique URL, watched before the request so no buffered or late entry hides it. */
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
      // An entry already buffered whose observer callback is still queued.
      if (protocol === undefined) take(performance.getEntriesByName(url, "resource"));
      return protocol ?? null;
    },
    stop: () => observer?.disconnect(),
  };
}

async function decoded<T>(
  response: Response,
  decode: (value: unknown) => T,
  message: string,
): Promise<T> {
  try {
    return decode(await readJSON(response));
  } catch (cause) {
    throw new CheckError(message, "retryable", { cause });
  }
}

/** A plain refusal's first line says why, such as a proxy that names no client; any other body is released unread. */
async function refusal(response: Response): Promise<string> {
  const body = response.headers.get("Content-Type")?.startsWith("text/plain")
    ? response.body
    : null;
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

/** A failure after the request left: the page's sign-in gets its one expiry check, then the failure is worded. */
async function settled(
  error: unknown,
  auth: ServerAuth,
  signal: AbortSignal,
  unreachable: string,
): Promise<never> {
  signal.throwIfAborted();
  if (auth.kind === "session" && !signsIn(error)) await auth.ended(signal);
  if (error instanceof CheckError || signsIn(error)) throw error;
  throw new CheckError(isNetworkFailure(error) ? unreachable : CHECK_FAILED, "retryable", {
    cause: error,
  });
}

/** One GET /preflight, decoded, held to the catalog entry's origins and classified for this page; no other side effect. */
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
      throw new CheckError(`${CHECK_FAILED} (HTTP ${response.status})`, "retryable");
    }
    const document = await decoded(response, preflight, `${CHECK_FAILED} (unreadable discovery)`);
    const requestMs = now() - startedAt;
    const { capabilities } = document;
    if (
      [...capabilities.throughput, ...capabilities.latency].some(
        (t) => t.baseUrl !== "." && !allowsOrigin(entry, t.baseUrl),
      )
    )
      throw new CheckError(`${entry.name} advertised an origin it may not use`, "retryable");
    const protocol = await timing.read(signal);
    return {
      generation: document.generation,
      engineVersion: document.engineVersion,
      server: document.server,
      maxStageMs: capabilities.maxStageMs,
      // Redirects are refused, so the answer came from the catalog's origin, which "." names.
      origin: entry.url,
      protocol,
      readAt: now(),
      requestMs,
      ...classify(capabilities, entry.url, page),
    };
  } catch (error) {
    // A protected HTTPS server refuses another origin without CORS headers, which reads as a network failure.
    const hint =
      entry.url.startsWith("https:") && !page.secure
        ? ". If it requires sign-in, open this interface over HTTPS."
        : "";
    return settled(error, auth, signal, `Could not reach this test server${hint}`);
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
      throw new CheckError(
        `Probe answered HTTP ${response.status}${await refusal(response)}`,
        "retryable",
      );
    const probe: Probe = await decoded(response, probeDocument, "Unexpected probe answer");
    return { probe, protocol: await timing.read(signal) };
  } catch (error) {
    return settled(error, auth, signal, `Could not reach ${origin}`);
  } finally {
    timing.stop();
  }
}

/** The targets a check tries: Automatic's candidates in rank order, or what an explicit choice resolves to. */
function targetsFor<R extends Role>(
  context: CheckContext,
  discovery: Discovery,
  role: R,
  selection: Selection,
) {
  const requested = resolve(discovery, role, selection, context.page);
  if (!requested) {
    const { message, retry } = unresolvedReason(discovery, role, selection, context.page);
    throw new CheckError(message, retry);
  }
  return selection.kind === "auto" ? candidates(discovery, role, context.page) : [requested];
}

/** Why Automatic passes over the target it prefers before trying any: the page's WebTransport throttle. */
function passedOver(
  context: CheckContext,
  discovery: Discovery,
  role: Role,
  selection: Selection,
): string | undefined {
  if (selection.kind !== "auto" || context.page.webTransport !== "throttled") return undefined;
  const head = resolve(discovery, role, selection, { ...context.page, webTransport: "available" });
  return head?.transport.startsWith("webtransport") ? THROTTLED : undefined;
}

/** The first target that verifies, told why any before it did not; Automatic stops at a sign-in refusal, and the preferred target's failure says the most. */
async function firstVerified<T extends ThroughputTarget | LatencyTarget, P>(
  targets: readonly T[],
  signal: AbortSignal,
  passed: string | undefined,
  verify: (target: T, signal: AbortSignal, fallback: string | undefined) => Promise<P>,
): Promise<P> {
  let failure: unknown;
  let retryable = false;
  for (const target of targets) {
    // Sockets add their establishment to the HTTP/3 bootstrap's wall time; each inner bound keeps its margin.
    const sockets = target.transport === "fetch-stream" ? 0 : ESTABLISH_MS + ESTABLISH_MARGIN_MS;
    const budgetMs = H3_DEADLINE_MS + ESTABLISH_MARGIN_MS + sockets;
    const fallback =
      failure === undefined
        ? passed
        : failure instanceof CheckError
          ? failure.message
          : CHECK_FAILED;
    try {
      return await withinBudget(signal, budgetMs, (bounded) => verify(target, bounded, fallback));
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

/** Proves the throughput path `selection` names: its origin answers a probe over the claimed protocol, and a session delivers bytes. */
export async function checkThroughput(
  context: CheckContext,
  discovery: Discovery,
  selection: Selection,
  signal: AbortSignal,
): Promise<ThroughputPath> {
  const targets = targetsFor(context, discovery, "throughput", selection);
  const verifiedFor = { selection: selectionText(selection), head: targets[0]!.id };
  const passed = passedOver(context, discovery, "throughput", selection);
  return firstVerified(targets, signal, passed, async (target, bounded, fallback) => ({
    ...(await verifyThroughput(context, discovery, target, bounded)),
    generation: discovery.generation,
    verifiedAt: context.now(),
    verifiedFor: { ...verifiedFor, fallback },
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
  const bounded =
    view.protocol === "http3"
      ? AbortSignal.any([signal, AbortSignal.timeout(H3_DEADLINE_MS)])
      : signal;
  let answer: { probe: Probe; protocol: string | null } | null = null;
  try {
    for (
      let attempt = 0;
      attempt < (session || view.protocol !== "http3" ? 1 : H3_ATTEMPTS);
      attempt++
    ) {
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
  // WebTransport proves its own data path; only its HTTP view's protocol is learned here.
  if (!session && !proven)
    throw new CheckError(
      observed
        ? `The browser used ${PROTOCOL_LABEL[observed]}, not ${PROTOCOL_LABEL[view.protocol]}`
        : `The browser did not confirm ${PROTOCOL_LABEL[view.protocol]}`,
      "retryable",
    );
  if (!proven || view.protocol === "negotiated") view.protocol = observed ?? "negotiated";
  if (session) await verifySession(context.auth, target, signal);
  return {
    target: session ? target : view,
    fetch: view,
    probe: answer!.probe,
    browserProtocol: answer!.protocol,
  };
}

/** Opening a session is not enough: the selected transfer must deliver a byte. */
async function verifySession(
  auth: ServerAuth,
  target: SessionTarget,
  signal: AbortSignal,
): Promise<void> {
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
    // Chrome counts a dial abandoned before it opened, and so does the page.
    if (session && !established) handshakes.failed();
    signal.throwIfAborted();
    const message = established
      ? "WebTransport session carried no bytes"
      : "WebTransport session did not establish";
    throw new CheckError(message, "retryable", { cause });
  } finally {
    clearTimeout(deadline);
    signal.removeEventListener("abort", close);
    close();
  }
}

/** The bus a latency check proved, kept open for the idle monitor to adopt; until then it closes with its check. */
export interface CheckedBus {
  /** Its events go to `events` from now on, and the channel to its adopter. */
  adopt(events: ProbeChannelEvents): ProbeChannel;
  dispose(): void;
}

/** Proves the latency path `selection` names: its origin answers a probe while its bus answers pings, both at once. */
export async function checkLatency(
  context: CheckContext,
  discovery: Discovery,
  selection: Selection,
  signal: AbortSignal,
): Promise<{ path: LatencyPath; bus: CheckedBus }> {
  const targets = targetsFor(context, discovery, "latency", selection);
  const verifiedFor = { selection: selectionText(selection), head: targets[0]!.id };
  const passed = passedOver(context, discovery, "latency", selection);
  return firstVerified(targets, signal, passed, async (target, bounded, fallback) => {
    // Either failing ends the other at once.
    const both = new AbortController();
    const linked = AbortSignal.any([bounded, both.signal]);
    const collecting = collectRtt(context, target, linked);
    try {
      const [{ probe }, { rttMs, channel, events }] = await Promise.all([
        probeOrigin(target.origin, context.auth, linked),
        collecting,
      ]);
      const release = () => channel.dispose();
      bounded.addEventListener("abort", release, { once: true });
      // The budget may have ended between the answers and here; its abort has fired already.
      if (bounded.aborted) release();
      const bus: CheckedBus = {
        adopt(next) {
          bounded.removeEventListener("abort", release);
          Object.assign(events, next);
          return channel;
        },
        dispose() {
          bounded.removeEventListener("abort", release);
          channel.dispose();
        },
      };
      const path = {
        target,
        probe,
        rttMs,
        generation: discovery.generation,
        verifiedAt: context.now(),
        verifiedFor: { ...verifiedFor, fallback },
      };
      return { path, bus };
    } catch (error) {
      void collecting.then(
        ({ channel }) => channel.dispose(),
        () => {},
      );
      throw error;
    } finally {
      both.abort();
    }
  });
}

type Collected = { rttMs: number | null; channel: ProbeChannel; events: ProbeChannelEvents };

/** Opens the bus at the preparation cadence and collects replies for a moment: their median or null, and the still-open bus. */
function collectRtt(
  context: CheckContext,
  target: LatencyTarget,
  signal: AbortSignal,
): Promise<Collected> {
  if (signal.aborted) return Promise.reject(signal.reason);
  const { promise, resolve, reject } = Promise.withResolvers<Collected>();
  const rtts: number[] = [];
  let channel: ProbeChannel | null = null;
  let collecting = false;
  let done = false;
  /** The median, once collected; handed over with the channel once that exists. */
  let collected = null as { rttMs: number | null } | null;
  const finish = (error?: unknown) => {
    if (done) return;
    done = true;
    clearTimeout(timer);
    signal.removeEventListener("abort", abort);
    if (error !== undefined) {
      channel?.dispose();
      return reject(error);
    }
    const sorted = rtts.toSorted((a, b) => a - b);
    collected = {
      rttMs: sorted.length ? midpointMedian(sorted.length, (rank) => sorted[rank]!) : null,
    };
    if (channel) resolve({ ...collected, channel, events });
  };
  const abort = () => finish(signal.reason);
  let timer = setTimeout(
    () => finish(new CheckError("Latency channel did not become ready", "retryable")),
    ESTABLISH_MS + ESTABLISH_MARGIN_MS,
  );
  signal.addEventListener("abort", abort, { once: true });
  const { entry, auth } = context;
  const events: ProbeChannelEvents = {
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
    signInRequired: () => finish(new SignInRequired(identityOf(entry))),
  };
  try {
    channel = context.openProbeChannel(
      { target, auth, intervalMs: PREPARATION_INTERVAL_MS, maxInFlight: 2 },
      events,
    );
  } catch (error) {
    finish(new CheckError("Latency channel did not open", "retryable", { cause: error }));
    return promise;
  }
  if (collected) resolve({ ...collected, channel, events });
  else if (done) channel.dispose();
  else channel.measure();
  return promise;
}
