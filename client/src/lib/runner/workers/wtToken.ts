import { readJSONResponse, parseWtToken } from "../../api/decode";
/* Minting happens in the worker, immediately before each dial, because the token expires in seconds and. */
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

/* How long an unspent token stays reusable when nothing reports spending it: one establish budget and the re-dial. */
const REUSE_WINDOW_MS = ESTABLISH_BUDGET_MS + LANE_RESTART_BACKOFF_MS;

interface WtToken {
  /** "" when minting failed or is not configured. */
  token: string;
  /** The refusal carried the auth marker, so the login session is gone. */
  authRequired: boolean;
}

/* INVARIANT: a token is spent by a CONNECT the server accepted, and nothing else. */
interface MintedToken {
  url: string;
  token: string;
  mintedAt: number;
  reusableForMs: number;
}

let held: MintedToken | null = null;

/** Report that a CONNECT carrying this token reached the server: it is spent, and the next dial has to mint its own. */
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

/* Hold a freshly minted token. */
function hold(url: string, token: string, expires: number | undefined): void {
  const lifetimeMs = typeof expires === "number" ? expires - Date.now() : 0;
  held = {
    url,
    token,
    mintedAt: Date.now(),
    reusableForMs: Math.max(0, Math.min(REUSE_WINDOW_MS, lifetimeMs)),
  };
}

/* Mint one CONNECT token. */
export async function mintWtToken(
  mint?: WtMint,
  signal?: AbortSignal,
): Promise<WtToken> {
  if (!mint || signal?.aborted) return { token: "", authRequired: false };
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
      // A hop that 302s a credentialed mint to a login page answers 200 with no token: refusing the redirect.
      redirect: redirectForCredentials(mint.credentials),
      signal: signal
        ? AbortSignal.any([signal, controller.signal])
        : controller.signal,
    });
    if (!res.ok)
      return { token: "", authRequired: authenticationRequired(res) };
    const { token, expires } = parseWtToken(await readJSONResponse(res));
    hold(mint.url, token, expires);
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
