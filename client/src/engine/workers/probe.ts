// One latency bus (api/wire.md); each connection has its own controller, so a replaced one cannot act.
import type { ProbeCommand, ProbeReport } from "../../protocol/messages";
import { Tickets, withToken } from "../lanes/dial";
import { abortable } from "../run/clock";
import { CLOSE_GRACE_MS, ESTABLISH_MS, REVOKED } from "../lanes/endings";
import { Prober } from "../lanes/prober";

type Start = Extract<ProbeCommand, { type: "start" }>;
type Send = (text: string) => void | Promise<void>;
/** Says how a connection ended, with a detail for a reconnect, or null when the server revoked the sign-in. */
type End = (detail: string | null) => void;

const tickets = new Tickets();
const encoder = new TextEncoder();
const decoder = new TextDecoder();
let prober: Prober;
let timer: ReturnType<typeof setTimeout> | undefined;
let connection: AbortController | null = null;
let send: Send | null = null;

self.onmessage = ({ data }: MessageEvent<ProbeCommand>) => {
  if (data.type === "start") start(data);
  else if (data.type === "measure") prober.measure();
  else if (data.type === "pace") prober.pace(data.intervalMs, data.maxInFlight);
  else prober.stop();
};

function start(command: Start): void {
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

async function dial({ url, transport, ticket }: Start): Promise<void> {
  const own = new AbortController();
  connection = own;
  const expired = AbortSignal.timeout(ESTABLISH_MS);
  const budget = AbortSignal.any([own.signal, expired]);
  let token = "";
  const end: End = (detail) => {
    if (own.signal.aborted) return;
    // An answer, a refusal too, spent the ticket. A WebTransport dial ended at its budget may never have reached the
    // server, while a WebSocket one always waits for the browser's answer.
    if (transport === "websocket" || !expired.aborted) tickets.spent(token);
    own.abort();
    if (connection === own) connection = send = null;
    if (detail === null) prober.signIn();
    else prober.closed(detail);
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

/** A refused upgrade closes as 1006, invisible to script. A dial past its budget stays open, reported as an outage:
    the browser may queue it behind another to the host, and closing it would only delay the next. */
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
    end(code === REVOKED.socket && reason === REVOKED.reason ? null : "websocket closed");
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
  let grace: ReturnType<typeof setTimeout> | undefined;
  closed.addEventListener("abort", () => {
    clearTimeout(grace);
    try {
      wt.close();
    } catch {}
  });
  wt.closed.then(
    ({ closeCode }) => end(closeCode === REVOKED.session ? null : "webtransport closed"),
    (error) => end(String(error)),
  );
  // Failing datagrams may be the session closing, whose code then names the ending.
  const broke = (detail: string) => void (grace ??= setTimeout(() => end(detail), CLOSE_GRACE_MS));
  let established = false;
  void (async () => {
    // A black-holed handshake can leave `ready` and `closed` pending, so the budget ends the dial.
    await abortable(wt.ready, budget);
    if (closed.aborted) return;
    established = true;
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
    established ? broke(String(error)) : end("webtransport did not establish"),
  );
}
