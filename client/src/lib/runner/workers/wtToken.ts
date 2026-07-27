/* A browser WebTransport CONNECT can send neither cookies nor headers, so an
 * authenticated dial first mints a single-use token over HTTP and carries it
 * in the session URL. Minting happens in the worker, immediately before each
 * dial, because the token expires in seconds and reconnects re-dial. */
import {
  authenticationRequired,
  redirectForCredentials,
} from "../../request-auth";
import { ESTABLISH_BUDGET_MS, LANE_RESTART_BACKOFF_MS } from "../real/budgets";

export interface WtMint {
  url: string;
  headers?: Record<string, string>;
  credentials?: RequestCredentials;
}

/** A mint that outlives the dial it feeds only parks a token server-side. */
const MINT_TIMEOUT_MS = 3000;

/** How long an unspent token stays reusable when nothing reports spending it:
 *  one establish budget and the re-dial that follows it, which is the span in
 *  which a second mint can only be the retry of a dial that never landed. */
const REUSE_WINDOW_MS = ESTABLISH_BUDGET_MS + LANE_RESTART_BACKOFF_MS;

export interface WtToken {
  /** "" when minting failed or is not configured. */
  token: string;
  /** The refusal carried the auth marker, so the login session is gone. */
  authRequired: boolean;
}

/** The token last minted for a URL, held for the retries of the dial it feeds.
 *
 *  INVARIANT: a token is spent by a CONNECT the server accepted, and by nothing
 *  else. A dial that fails before that -- UDP blocked, a listener restarting --
 *  leaves it valid until its own expiry, so minting again for the retry parks a
 *  second token against the session's cap of eight, which every other stage and
 *  tab of the same login draws on. Reuse therefore ends at the first of:
 *  spendWtToken, the lifetime the server reported, and REUSE_WINDOW_MS, the
 *  bound that holds when no caller reports a spend.
 *
 *  This is realm state, so it only spans dials a single worker makes. The
 *  transfer workers are respawned per restart and never see it twice; the ping
 *  bus re-dials in the realm it is already running in, which is why every dial
 *  path has to report its spend for the invariant to hold anywhere. */
interface MintedToken {
  url: string;
  token: string;
  mintedAt: number;
  reusableForMs: number;
}

let held: MintedToken | null = null;

/** Report that a CONNECT carrying this token reached the server: it is spent,
 *  and the next dial has to mint its own. */
export function spendWtToken(token: string): void {
  if (held?.token === token) held = null;
}

/** The held token, while it is still this URL's and still reusable. */
function reusableToken(url: string): string {
  if (!held || held.url !== url) return "";
  if (Date.now() - held.mintedAt >= held.reusableForMs) {
    held = null;
    return "";
  }
  return held.token;
}

/** Hold a freshly minted token. The server's expiry becomes a duration at the
 *  moment it arrives rather than staying an instant, so a client clock offset
 *  from the server's cannot stretch the lifetime it stands for. */
function hold(url: string, token: string, expires: unknown): void {
  const lifetimeMs = typeof expires === "number" ? expires - Date.now() : 0;
  held = {
    url,
    token,
    mintedAt: Date.now(),
    reusableForMs: Math.max(0, Math.min(REUSE_WINDOW_MS, lifetimeMs)),
  };
}

/** Mint one CONNECT token. The refusal classifies itself, so a caller needs no
 *  second request to tell an expired session from an unreachable server. */
export async function mintWtToken(
  mint?: WtMint,
  signal?: AbortSignal,
): Promise<WtToken> {
  if (!mint) return { token: "", authRequired: false };
  const reused = reusableToken(mint.url);
  if (reused !== "") return { token: reused, authRequired: false };
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), MINT_TIMEOUT_MS);
  try {
    const res = await fetch(mint.url, {
      method: "POST",
      cache: "no-store",
      headers: mint.headers,
      credentials: mint.credentials,
      // A hop that 302s a credentialed mint to a login page answers 200 with no
      // token: refusing the redirect classifies it instead of failing on the
      // body, which reads as an unreachable server and reconnects forever.
      redirect: redirectForCredentials(mint.credentials),
      signal: signal
        ? AbortSignal.any([signal, controller.signal])
        : controller.signal,
    });
    if (!res.ok)
      return { token: "", authRequired: authenticationRequired(res) };
    const body = (await res.json()) as { token?: unknown; expires?: unknown };
    const token = typeof body.token === "string" ? body.token : "";
    if (token !== "") hold(mint.url, token, body.expires);
    return { token, authRequired: false };
  } catch {
    return { token: "", authRequired: false };
  } finally {
    clearTimeout(timeout);
  }
}

/** Append the token to a session URL; a blank token leaves the URL alone. */
export function withWtToken(url: string, token: string): string {
  if (token === "") return url;
  return `${url}${url.includes("?") ? "&" : "?"}token=${encodeURIComponent(token)}`;
}
