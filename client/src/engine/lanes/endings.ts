// How lanes, feeds and buses end, in the eight failure reasons; workers classify, owners decide.
import { release, signInMarker } from "../../protocol/documents";
import { ROUTES } from "../../protocol/routes";
import type { Ending, ServerAuth } from "../types";
import { delay } from "../run/clock";

/** A connection that has not answered by now is not going to. */
export const ESTABLISH_MS = 3_000;
export const RESTART_MS = 300;
/** Retry-After raises the pause up to here, so the one retry still lands inside the run's 1.5 s silence window. */
export const RESTART_CAP_MS = 1_200;

type Disposition = Pick<Ending, "reason" | "retry" | "rotate" | "bound"> & { detail?: string };
const LOST: Disposition = { reason: "connection-lost", retry: true };
const LIMITED: Disposition = { reason: "client-limit", retry: true };
const BUSY: Disposition = { reason: "server-capacity", retry: true };
const SIGN_IN: Disposition = { reason: "sign-in-required", retry: false };
const UNEXPECTED: Disposition = { reason: "protocol-error", retry: false };
/** Refusals by code (api/refusals.txt); `invalid` means the server forgot the upload id, which may be replaced once. */
const REFUSALS: Record<string, Disposition> = {
  invalid: { ...UNEXPECTED, rotate: true },
  "owner-mismatch": UNEXPECTED,
  idle: { reason: "timeout", retry: true, bound: true },
  revoked: SIGN_IN,
  "client-limit": LIMITED,
  "server-capacity": BUSY,
  "wrong-protocol": {
    reason: "path-failed",
    retry: false,
    detail: "this port serves transfers over HTTP/3, not TCP",
  },
};
const coded = (code: string | null | undefined) =>
  code && Object.hasOwn(REFUSALS, code) ? REFUSALS[code]! : null;
/** Without a code, the status says only who refused: this client's limit, or a busy server. */
const admission = (status: number | null) =>
  status === 429 ? LIMITED : status === 503 ? BUSY : null;

export const lost = (detail: string): Ending => ({ ...LOST, detail });

/** A refusal, by its code or else its status; a bare 401 is a proxy's challenge, never an ended sign-in. */
export function refusal(
  status: number | null,
  code: string | null | undefined,
  detail: string,
  retryAfterMs?: number,
): Ending {
  const disposition =
    coded(code) ??
    admission(status) ??
    (status !== null && (status === 0 || status === 408 || status >= 500) ? LOST : UNEXPECTED);
  return { detail, ...disposition, retryAfterMs };
}

/** Retry-After in whole positive seconds; a date or a fraction is ignored. */
const retryAfterMs = ({ headers }: Response) => {
  const seconds = headers.get("Retry-After") ?? "";
  return /^\d+$/.test(seconds) && +seconds > 0 ? +seconds * 1000 : undefined;
};

const REFUSAL_HEADER = "Graphite-Meter-Refusal";

/** A refused upload, progress feed or lease. */
export const refused = (response: Response, detail = `HTTP ${response.status}`): Ending =>
  signInMarker(response)
    ? { ...SIGN_IN, detail }
    : refusal(
        response.status,
        response.headers.get(REFUSAL_HEADER),
        detail,
        retryAfterMs(response),
      );

/** Only an admission refusal reconnects a download; any status without a code is an unexpected answer. */
export const downloadRefused = (response: Response): Ending => ({
  detail: `HTTP ${response.status}`,
  ...(signInMarker(response)
    ? SIGN_IN
    : (coded(response.headers.get(REFUSAL_HEADER)) ?? admission(response.status) ?? UNEXPECTED)),
  retryAfterMs: retryAfterMs(response),
});

/** Close codes of api/laneendings.txt: revocation ends, idle and lifetime bounds reconnect, others are breaks. */
export const REVOKED = { socket: 1008, session: 3, reason: "authentication required" } as const;
const SESSION_BOUNDS = [1, 2];
const SOCKET_BOUNDS = [4001, 4002];
/** A server resets a session's streams just before closing it; a broken stream waits this long for the code. */
export const CLOSE_GRACE_MS = 250;

/** A WebTransport session's close code as an ending, or null for a break. */
export const sessionClosed = (code: number | undefined): Ending | null =>
  code === REVOKED.session
    ? { ...SIGN_IN, detail: "webtransport session revoked" }
    : SESSION_BOUNDS.includes(code ?? -1)
      ? {
          reason: "timeout",
          retry: true,
          bound: true,
          detail: "webtransport session reached its bound",
        }
      : null;

/** A WebSocket closed at the server's idle or lifetime bound, as an ending, or null. */
export const socketBound = (code: number, label: string): Ending | null =>
  SOCKET_BOUNDS.includes(code)
    ? { reason: "timeout", retry: true, bound: true, detail: `${label} reached its bound` }
    : null;

/** One lane's or feed's restart pause: 300 ms, raised by Retry-After within 1.2 s. */
export const restartMs = (ending: Ending) =>
  Math.min(RESTART_CAP_MS, Math.max(RESTART_MS, ending.retryAfterMs ?? 0));

const ANSWER_RETRY_MS = 500;

/** Asks the server over HTTP every half second until it answers or `withinMs` passed: what a reopen or redial waits for. */
export async function answered(
  origin: string,
  auth: ServerAuth,
  signal: AbortSignal,
  withinMs = Infinity,
): Promise<boolean> {
  const giveUp = performance.now() + withinMs;
  while (!signal.aborted) {
    if (await answers(origin, auth, signal)) return true;
    if (performance.now() >= giveUp) return false;
    await delay(ANSWER_RETRY_MS, signal);
  }
  return false;
}

/** Whether the server answers a bounded GET /probe at `origin`. */
async function answers(origin: string, auth: ServerAuth, signal: AbortSignal): Promise<boolean> {
  try {
    const response = await auth.fetch(`${origin}${ROUTES.probe}`, {
      cache: "no-store",
      signal: AbortSignal.any([signal, AbortSignal.timeout(ESTABLISH_MS)]),
    });
    release(response);
    return response.ok;
  } catch {
    return false;
  }
}
