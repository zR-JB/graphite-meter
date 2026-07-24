/* A browser WebTransport CONNECT can send neither cookies nor headers, so an
 * authenticated dial first mints a single-use token over HTTP and carries it
 * in the session URL. Minting happens in the worker, immediately before each
 * dial, because the token expires in seconds and reconnects re-dial. */

export interface WtMint {
  url: string;
  headers?: Record<string, string>;
  credentials?: RequestCredentials;
}

/** Mint one CONNECT token; "" when minting fails or is not configured. */
export async function mintWtToken(mint?: WtMint): Promise<string> {
  if (!mint) return "";
  try {
    const res = await fetch(mint.url, {
      method: "POST",
      cache: "no-store",
      headers: mint.headers,
      credentials: mint.credentials,
    });
    if (!res.ok) return "";
    const body = (await res.json()) as { token?: unknown };
    return typeof body.token === "string" ? body.token : "";
  } catch {
    return "";
  }
}

/** Append the token to a session URL; a blank token leaves the URL alone. */
export function withWtToken(url: string, token: string): string {
  if (token === "") return url;
  return `${url}${url.includes("?") ? "&" : "?"}token=${encodeURIComponent(token)}`;
}
