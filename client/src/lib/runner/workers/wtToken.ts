/* A browser WebTransport CONNECT can send neither cookies nor headers, so an
 * authenticated dial first mints a single-use token over HTTP and carries it
 * in the session URL. Minting happens in the worker, immediately before each
 * dial, because the token expires in seconds and reconnects re-dial. */
import {
  authenticationRequired,
  redirectForCredentials,
} from "../../request-auth";

export interface WtMint {
  url: string;
  headers?: Record<string, string>;
  credentials?: RequestCredentials;
}

/** A mint that outlives the dial it feeds only parks a token server-side. */
const MINT_TIMEOUT_MS = 3000;

export interface WtToken {
  /** "" when minting failed or is not configured. */
  token: string;
  /** The refusal carried the auth marker, so the login session is gone. */
  authRequired: boolean;
}

/** Mint one CONNECT token. The refusal classifies itself, so a caller needs no
 *  second request to tell an expired session from an unreachable server. */
export async function mintWtToken(
  mint?: WtMint,
  signal?: AbortSignal,
): Promise<WtToken> {
  if (!mint) return { token: "", authRequired: false };
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
    const body = (await res.json()) as { token?: unknown };
    return {
      token: typeof body.token === "string" ? body.token : "",
      authRequired: false,
    };
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
