// One latency bus (api/wire.md); each connection has its own controller, so a replaced one cannot act.
import type { ProbeCommand, ProbeReport } from "../../protocol/messages";
import { Tickets, withToken } from "../lanes/dial";
import { abortable } from "../run/clock";
import {
  CLOSE_GRACE_MS,
  ESTABLISH_MS,
  REVOKED,
  sessionClosed,
  socketBound,
} from "../lanes/endings";
import { Prober } from "../lanes/prober";

type Start = Extract<ProbeCommand, { type: "start" }>;
type Send = (text: string) => void | Promise<void>;
/** Says how a connection ended, with a detail for a reconnect, or null when the server revoked the sign-in; at the server's idle or lifetime bound it dials again at once. */
type End = (detail: string | null, bound?: boolean) => void;

const tickets = new Tickets();
const encoder = new TextEncoder();
const decoder = new TextDecoder();
let prober: Prober;
let timer: ReturnType<typeof setTimeout> | undefined;
let connection: AbortController | null = null;
/** The owner asked to dial while a lapsed dial still waits for the browser's verdict, so the dial follows its end. */
let queued = false;
let send: Send | null = null;

let command: Start;

self.onmessage = ({ data }: MessageEvent<ProbeCommand>) => {
  if (data.type === "start") start(data);
  else if (data.type === "dial") {
    if (connection) queued = true;
    else void dial(command);
  } else if (data.type === "renew") renew();
  else if (data.type === "measure") prober.measure();
  else if (data.type === "pace") prober.pace(data.intervalMs, data.maxInFlight);
  else prober.stop(data.id);
};

function start(started: Start): void {
  command = started;
  const clock = {
    origin: performance.timeOrigin,
    now: () => performance.now(),
    arm(ms: number) {
      clearTimeout(timer);
      timer = setTimeout(() => prober.wake(), ms);
    },
    disarm: () => clearTimeout(timer),
  };
  prober = new Prober(command, clock, {
    dial: () => void dial(command),
    send: (text) => send!(text),
    hangUp() {
      connection?.abort();
      connection = send = null;
    },
    post: (report: ProbeReport) => postMessage(report),
  });
  prober.start();
}

function renew(): void {
  if (!send) return;
  connection?.abort();
  connection = send = null;
  redial();
}

function redial(): void {
  prober.renewing();
  postMessage({ type: "renewing" } satisfies ProbeReport);
  void dial(command);
}

async function dial({ url, transport, ticket }: Start): Promise<void> {
  const own = new AbortController();
  connection = own;
  const expired = AbortSignal.timeout(ESTABLISH_MS);
  const budget = AbortSignal.any([own.signal, expired]);
  let token = "";
  let openedAt = Infinity;
  const end: End = (detail, bound = false) => {
    if (own.signal.aborted) return;
    // Any answer spent the ticket; a WebTransport dial ended at its budget may never have reached the server, unlike a WebSocket one.
    if (transport === "websocket" || !expired.aborted) tickets.spent(token);
    own.abort();
    if (connection === own) connection = send = null;
    if (detail === null) return prober.signIn();
    if (bound && !connection && performance.now() - openedAt >= ESTABLISH_MS) return redial();
    prober.closed(detail);
    if (queued && !connection) {
      queued = false;
      void dial(command);
    }
  };
  if (ticket) {
    const minted = await tickets.take(ticket, budget);
    if (own.signal.aborted) return;
    if (minted.type === "ended" && minted.ending.reason === "sign-in-required") return end(null);
    if (minted.type !== "token")
      return end(
        budget.aborted
          ? `${transport} did not establish`
          : minted.type === "ended"
            ? minted.ending.detail
            : minted.detail,
      );
    token = minted.token;
  }
  const opened = (write: Send) => {
    if (own.signal.aborted) return;
    openedAt = performance.now();
    queued = false;
    tickets.spent(token);
    send = write;
    prober.opened();
  };
  try {
    if (transport === "websocket") socket(withToken(url, token), own.signal, expired, end, opened);
    else session(withToken(url, token), own.signal, budget, end, opened);
  } catch (error) {
    end(String(error));
  }
}

/** A refused upgrade closes as 1006, invisible to script; a dial past its budget stays open as an outage, since closing would delay the next. */
function socket(
  url: string,
  closed: AbortSignal,
  expired: AbortSignal,
  end: End,
  opened: (write: Send) => void,
): void {
  const ws = new WebSocket(url);
  closed.addEventListener("abort", () => ws.close());
  const lapse = () => prober.lapsed("websocket did not establish");
  expired.addEventListener("abort", lapse, { signal: closed });
  ws.onopen = () => {
    expired.removeEventListener("abort", lapse);
    opened((text) => ws.send(text));
  };
  ws.onmessage = ({ data }: MessageEvent<unknown>) => {
    const at = performance.now();
    if (!closed.aborted && typeof data === "string") prober.frame(data, at);
  };
  // An error always precedes a close, the one place a socket ends.
  ws.onclose = ({ code, reason }) =>
    end(
      code === REVOKED.socket && reason === REVOKED.reason ? null : "websocket closed",
      socketBound(code, "bus") !== null,
    );
}

/** One wire message per datagram, so reading needs no framing. */
function session(
  url: string,
  closed: AbortSignal,
  budget: AbortSignal,
  end: End,
  opened: (write: Send) => void,
): void {
  const wt = new WebTransport(url, { congestionControl: "low-latency" });
  let established = false;
  let settled = false;
  // A dial that ends before it opened failed its handshake, at its budget or hung up too, as Chrome counts it.
  const ended: End = (detail, bound) => {
    if (!established && !settled)
      postMessage({ type: "handshake", opened: false } satisfies ProbeReport);
    settled = true;
    end(detail, bound);
  };
  let grace: ReturnType<typeof setTimeout> | undefined;
  closed.addEventListener("abort", () => {
    clearTimeout(grace);
    try {
      wt.close();
    } catch {}
  });
  wt.closed.then(
    ({ closeCode }) =>
      ended(
        closeCode === REVOKED.session ? null : "webtransport closed",
        !!sessionClosed(closeCode)?.bound,
      ),
    (error) => ended(String(error)),
  );
  // Failing datagrams may be the session closing, whose code then names the ending.
  const broke = (detail: string) => void (grace ??= setTimeout(() => end(detail), CLOSE_GRACE_MS));
  void (async () => {
    // A black-holed handshake can leave `ready` and `closed` pending, so the budget ends the dial.
    await abortable(wt.ready, budget);
    if (closed.aborted) return;
    established = true;
    postMessage({ type: "handshake", opened: true } satisfies ProbeReport);
    const writer = wt.datagrams.writable.getWriter();
    opened((text) => writer.write(encoder.encode(text)));
    const reader = wt.datagrams.readable.getReader();
    for (;;) {
      const { value, done } = await reader.read();
      if (closed.aborted) return;
      if (done) return broke("webtransport datagrams closed");
      const at = performance.now();
      prober.frame(decoder.decode(value), at);
    }
  })().catch((error) =>
    established ? broke(String(error)) : ended("webtransport did not establish"),
  );
}
