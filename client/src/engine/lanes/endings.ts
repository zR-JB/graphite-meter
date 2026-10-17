// Classifies how lanes, feeds and buses end into the seven failure reasons, and says how soon one may reopen. This
// module is pure. The workers classify the answers they see, and the owners decide what an ending means.
import { signInMarker } from "../../protocol/documents";
import type { Ending } from "../types";

/** A connection that has not answered by now is not going to. A dial's ticket and handshake share this budget. */
export const ESTABLISH_MS = 3_000;
const RESTART_MS = 300;
/** Busy refusals double the pause up to here, so a retry still lands inside the run's 1.5 s silence window. */
const BUSY_RESTART_CAP_MS = 1_200;

type Disposition = Pick<Ending, "reason" | "retry" | "rotate">;
const LOST: Disposition = { reason: "connection-lost", retry: true };
const BUSY: Disposition = { reason: "server-busy", retry: true };
const SIGN_IN: Disposition = { reason: "sign-in-required", retry: false };
const UNEXPECTED: Disposition = { reason: "protocol-error", retry: false };
/** Maps the upload refusals (api/uploadrefusals.txt) to their reasons (api/uploadrefusalreasons.txt). `invalid` means
    the server no longer knows the id, which the run may replace once. */
const REFUSALS: Record<string, Disposition> = {
  invalid: { ...UNEXPECTED, rotate: true },
  ownerMismatch: UNEXPECTED,
  globalFull: BUSY,
  clientFull: BUSY,
  idle: { reason: "timeout", retry: true },
  revoked: SIGN_IN,
};

export const lost = (detail: string): Ending => ({ ...LOST, detail });

/** Classifies an upload refusal. Its code decides, or without one its status does, where a network-level or generic
    server failure reconnects the same id. A bare 401 is a proxy's challenge, an unexpected answer, never an ended
    sign-in. A feed's error record has no status. */
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

/** Reads Retry-After in whole positive seconds. A date or a fraction is ignored. */
const retryAfterMs = ({ headers }: Response) => {
  const seconds = headers.get("Retry-After") ?? "";
  return /^\d+$/.test(seconds) && +seconds > 0 ? +seconds * 1000 : undefined;
};

/** Classifies a refused upload, progress feed or socket ticket. */
export const refused = (response: Response, detail = `HTTP ${response.status}`): Ending =>
  signInMarker(response)
    ? { ...SIGN_IN, detail }
    : refusal(response.status, response.headers.get("X-Graphite-Upload-Refusal"), detail, retryAfterMs(response));

/** Only an admission refusal reconnects a download. Any other status is an unexpected answer. */
export const downloadRefused = (response: Response): Ending => ({
  ...(signInMarker(response) ? SIGN_IN : response.status === 429 || response.status === 503 ? BUSY : UNEXPECTED),
  detail: `HTTP ${response.status}`,
  retryAfterMs: retryAfterMs(response),
});

/** A server names these endings when it closes a bus or session (api/laneendings.txt). The browser acts on revocation
    and reconnects after the idle and lifetime bounds. Any other close is a break. */
export const REVOKED = { socket: 1008, session: 3, reason: "authentication required" } as const;
const SESSION_BOUNDS = [1, 2];
/** A server resets a session's streams just before it closes it, and closing first would replace its code with ours.
    So a broken stream waits this long for the code that names the ending. */
export const CLOSE_GRACE_MS = 250;

/** Turns a WebTransport session's close code into an ending, or null for a break. */
export const sessionClosed = (code: number | undefined): Ending | null =>
  code === REVOKED.session
    ? { ...SIGN_IN, detail: "webtransport session revoked" }
    : SESSION_BOUNDS.includes(code ?? -1)
      ? { reason: "timeout", retry: true, detail: "webtransport session reached its bound" }
      : null;

/** Paces one lane's or feed's restarts. The pause starts at 300 ms, doubles with consecutive busy refusals and rises
    with a Retry-After, within 1.2 s. */
export class Backoff {
  #busy = 0;

  after(ending: Ending): number {
    const streak = ending.reason === "server-busy" ? this.#busy : 0;
    this.#busy = ending.reason === "server-busy" ? streak + 1 : 0;
    return Math.min(BUSY_RESTART_CAP_MS, Math.max(RESTART_MS * 2 ** streak, ending.retryAfterMs ?? 0));
  }

  /** Delivery ends a busy streak. */
  reset(): void {
    this.#busy = 0;
  }

  /** The lane was refused as busy since it last delivered. */
  get busy(): boolean {
    return this.#busy > 0;
  }
}
