// How lanes, feeds and buses end, in the seven failure reasons; workers classify, owners decide.
import { signInMarker } from "../../protocol/documents";
import type { Ending } from "../types";

/** A connection that has not answered by now is not going to; a dial's ticket and handshake share it. */
export const ESTABLISH_MS = 3_000;
export const RESTART_MS = 300;
/** Busy refusals double the pause up to here, so a retry still lands inside the run's 1.5 s silence window. */
const BUSY_RESTART_CAP_MS = 1_200;

type Disposition = Pick<Ending, "reason" | "retry" | "rotate">;
const LOST: Disposition = { reason: "connection-lost", retry: true };
const BUSY: Disposition = { reason: "server-busy", retry: true };
const SIGN_IN: Disposition = { reason: "sign-in-required", retry: false };
const UNEXPECTED: Disposition = { reason: "protocol-error", retry: false };
/** Upload refusals (api/uploadrefusals.txt); `invalid` means the server forgot the id, which may be replaced once. */
const REFUSALS: Record<string, Disposition> = {
  invalid: { ...UNEXPECTED, rotate: true },
  ownerMismatch: UNEXPECTED,
  globalFull: BUSY,
  clientFull: BUSY,
  idle: { reason: "timeout", retry: true },
  revoked: SIGN_IN,
};

export const lost = (detail: string): Ending => ({ ...LOST, detail });

/** An upload refusal, by its code or else its status; a bare 401 is a proxy's challenge, never an ended sign-in. */
export function refusal(
  status: number | null,
  code: string | null | undefined,
  detail: string,
  retryAfterMs?: number,
): Ending {
  const disposition =
    code && Object.hasOwn(REFUSALS, code)
      ? REFUSALS[code]!
      : status === 429 || status === 503
        ? BUSY
        : status !== null && (status === 0 || status === 408 || status >= 500)
          ? LOST
          : UNEXPECTED;
  return { ...disposition, detail, retryAfterMs };
}

/** Retry-After in whole positive seconds; a date or a fraction is ignored. */
const retryAfterMs = ({ headers }: Response) => {
  const seconds = headers.get("Retry-After") ?? "";
  return /^\d+$/.test(seconds) && +seconds > 0 ? +seconds * 1000 : undefined;
};

/** A refused upload, progress feed or socket ticket. */
export const refused = (response: Response, detail = `HTTP ${response.status}`): Ending =>
  signInMarker(response)
    ? { ...SIGN_IN, detail }
    : refusal(
        response.status,
        response.headers.get("X-Graphite-Upload-Refusal"),
        detail,
        retryAfterMs(response),
      );

/** Only an admission refusal reconnects a download; any other status is an unexpected answer. */
export const downloadRefused = (response: Response): Ending => ({
  ...(signInMarker(response)
    ? SIGN_IN
    : response.status === 429 || response.status === 503
      ? BUSY
      : UNEXPECTED),
  detail: `HTTP ${response.status}`,
  retryAfterMs: retryAfterMs(response),
});

/** Close codes of api/laneendings.txt: revocation ends, idle and lifetime bounds reconnect, others are breaks. */
export const REVOKED = { socket: 1008, session: 3, reason: "authentication required" } as const;
const SESSION_BOUNDS = [1, 2];
/** A server resets a session's streams just before closing it; a broken stream waits this long for the code. */
export const CLOSE_GRACE_MS = 250;

/** A WebTransport session's close code as an ending, or null for a break. */
export const sessionClosed = (code: number | undefined): Ending | null =>
  code === REVOKED.session
    ? { ...SIGN_IN, detail: "webtransport session revoked" }
    : SESSION_BOUNDS.includes(code ?? -1)
      ? { reason: "timeout", retry: true, detail: "webtransport session reached its bound" }
      : null;

/** One lane's or feed's restart pause: 300 ms, doubled by consecutive busy refusals and raised by a Retry-After,
    within 1.2 s. */
export class Backoff {
  #busy = 0;

  after(ending: Ending): number {
    const streak = ending.reason === "server-busy" ? this.#busy : 0;
    this.#busy = ending.reason === "server-busy" ? streak + 1 : 0;
    return Math.min(
      BUSY_RESTART_CAP_MS,
      Math.max(RESTART_MS * 2 ** streak, ending.retryAfterMs ?? 0),
    );
  }

  /** Delivery ends a busy streak. */
  reset(): void {
    this.#busy = 0;
  }

  /** Refused as busy since it last delivered. */
  get busy(): boolean {
    return this.#busy > 0;
  }
}
