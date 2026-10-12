// Per-server credentials: none, the page's own login, or a measurement grant a peer approved. Requests they authorize
// stay on the server's allowed destinations, a grant only on its issuer's HTTPS hostname and never in a URL, and every
// lifetime is a monotonic deadline, a grant's also a wall-clock one.
import { grant as grantDocument, readJSON, type CatalogEntry } from "../../protocol/documents";
import { ROUTES } from "../../protocol/routes";
import { SignInRequired, type RequestRecipe, type ServerAuth, type ServerIdentity } from "../types";
import { CheckError, delay } from "./checks";
import { signInMarker, type PageConfig, type PageSession } from "./page";
import { allowsOrigin } from "./paths";

/** A measurement grant, held in memory only: its bearer token and when its parent login ends. */
export interface Grant {
  token: string;
  /** Monotonic page ms. */
  deadline: number;
  /** Epoch ms by this page's clock, which runs on through a suspend that can pause the monotonic one. */
  expires: number;
  maximumLifetimeMs: number;
}

/** How long `grant` has left, by whichever clock says less. */
export const grantLeft = (grant: Grant, now: () => number): number =>
  Math.min(grant.deadline - now(), grant.expires - Date.now());

const ENDED_CHECK_MS = 3_000;

const identityOf = (entry: CatalogEntry): ServerIdentity => ({
  id: entry.id,
  url: entry.url,
  name: entry.name,
  ...(entry.location ? { location: entry.location } : {}),
});

function credentials(
  entry: CatalogEntry,
  kind: ServerAuth["kind"],
  deadline: number | null,
  recipe: (target: URL, method: string) => RequestRecipe,
  ended: ServerAuth["ended"],
  refused: () => void,
): ServerAuth {
  const request = (url: string, method: string): RequestRecipe => {
    if (!allowsOrigin(entry, url))
      throw new CheckError("Request destination is outside the selected server", "impossible");
    return recipe(new URL(url), method.toUpperCase());
  };
  return {
    kind,
    deadline,
    request,
    async fetch(url, init = {}) {
      const { headers, credentials, redirect } = request(url, init.method ?? "GET");
      const merged = new Headers(init.headers);
      for (const [name, value] of Object.entries(headers)) merged.set(name, value);
      // Checkpoints and session control must stay responsive during bulk transfers.
      const response = await fetch(url, { priority: "high", ...init, headers: merged, credentials, redirect });
      if (init.signal?.aborted || !signInMarker(response)) return response;
      await response.body?.cancel().catch(() => {});
      refused();
      throw new SignInRequired(identityOf(entry));
    },
    ticket(origin, route) {
      const socket = route.startsWith("/ws/");
      // A WebSocket carries the page's own cookie; WebTransport carries neither cookie nor header, only a ticket.
      if (kind === "public" || (kind === "session" && socket)) return null;
      const url = `${origin}${socket ? ROUTES.wsSession : ROUTES.wtSession}?target=${encodeURIComponent(origin + route)}`;
      return { url, request: request(url, "POST") };
    },
    ended,
  };
}

export const publicAuth = (entry: CatalogEntry): ServerAuth =>
  credentials(
    entry,
    "public",
    null,
    () => ({ headers: {}, credentials: "omit", redirect: "error" }),
    async () => false,
    () => {},
  );

export function sessionAuth(entry: CatalogEntry, page: PageConfig, session: PageSession): ServerAuth {
  const host = new URL(page.origin).hostname;
  return credentials(
    entry,
    "session",
    null,
    (target, method) => {
      // The page's cookies and CSRF token reach only its own secure hostname, never an additional origin.
      if (target.origin !== page.origin && (target.protocol !== "https:" || target.hostname !== host))
        throw new CheckError("Session credentials stay on this page's secure hostname", "impossible");
      return { headers: session.headers(method), credentials: "include", redirect: "error" };
    },
    (signal) => session.expired(signal),
    () => session.end("expired"),
  );
}

export function grantAuth(entry: CatalogEntry, grant: Grant, now: () => number): ServerAuth {
  const issuer = new URL(entry.url).hostname;
  const authorization = { Authorization: `Bearer ${grant.token}` };
  let revoked = false;
  let checking: Promise<boolean> | null = null;
  const over = () => revoked || grantLeft(grant, now) <= 0;
  // A broken stream is how a withdrawn grant first shows: the grant's own request is refused with the marker.
  async function check(): Promise<boolean> {
    try {
      const response = await fetch(`${entry.url}${ROUTES.probe}`, {
        cache: "no-store",
        credentials: "omit",
        redirect: "error",
        headers: authorization,
        signal: AbortSignal.timeout(ENDED_CHECK_MS),
      });
      response.body?.cancel().catch(() => {});
      return (revoked ||= signInMarker(response));
    } catch {
      return false;
    }
  }
  return credentials(
    entry,
    "grant",
    grant.deadline,
    (target) => {
      if (target.protocol !== "https:") throw new CheckError("Measurement grants require HTTPS", "impossible");
      // The issuer accepts its grant only on its own hostname, never on an additional origin.
      if (target.hostname !== issuer)
        throw new CheckError("Measurement grants stay on their server's hostname", "impossible");
      if (over()) throw new SignInRequired(identityOf(entry));
      return { headers: { ...authorization }, credentials: "omit", redirect: "error" };
    },
    async (signal) => {
      if (over()) return true;
      if (signal?.aborted) return false;
      checking ??= check().finally(() => (checking = null));
      return (await checking) && !signal?.aborted;
    },
    () => (revoked = true),
  );
}

/** The page's own server rides the page's login when there is one; every other server starts public. */
export const initialAuth = (entry: CatalogEntry, page: PageConfig, session: PageSession): ServerAuth =>
  entry.id === "self" && entry.url === page.origin && page.auth ? sessionAuth(entry, page, session) : publicAuth(entry);

const base64url = (bytes: Uint8Array) =>
  btoa(String.fromCharCode(...bytes))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replaceAll("=", "");
const BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/** The approval's public challenge, and the comparison code both pages show: RFC 4648 base32 of the first five
    bytes of the verifier's SHA-256, eight characters. */
export async function approvalChallenge(verifier: string): Promise<{ challenge: string; code: string }> {
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)));
  let code = "";
  let value = 0;
  let bits = 0;
  for (const byte of hash.subarray(0, 5)) {
    value = (value << 8) | byte;
    for (bits += 8; bits >= 5; bits -= 5) code += BASE32.charAt((value >>> (bits - 5)) & 31);
    value &= (1 << bits) - 1;
  }
  return { challenge: base64url(hash), code };
}

/** A fresh approval for `entry`: the page to open, the code to compare, and the verifier, which never leaves this
    page except in the exchange's body. */
export async function beginApproval(
  entry: CatalogEntry,
  page: PageConfig,
): Promise<{ url: string; code: string; verifier: string }> {
  if (!page.secure || !entry.url.startsWith("https://"))
    throw new Error("Open this interface over HTTPS to authorize a remote server");
  const verifier = base64url(crypto.getRandomValues(new Uint8Array(32)));
  const { challenge, code } = await approvalChallenge(verifier);
  const query = new URLSearchParams({ challenge, client_origin: page.origin });
  return { url: `${entry.url}/auth/browser?${query}`, code, verifier };
}

export class ApprovalLimitError extends Error {
  constructor() {
    super("This server login has reached its limit of authorized clients.");
  }
}

/** The server keeps an approval this long. */
const APPROVAL_MS = 120_000;
const MAX_GRANT_MS = 8 * 3_600_000;
const POLL_MS = 1_000;
const EXCHANGE_TIMEOUT_MS = 5_000;

/** Polls the peer's verifier-bound exchange until the user approves, within the approval's lifetime. A network
    failure, a slow answer or a failing hop is retried in the next round, which the server answers with the grant it
    already issued for 30 s; a refusal is final. */
export async function exchangeGrant(
  entry: CatalogEntry,
  verifier: string,
  signal: AbortSignal,
  now: () => number,
): Promise<Grant> {
  const deadline = now() + APPROVAL_MS;
  while (now() < deadline) {
    signal.throwIfAborted();
    const [sentAt, sentWall] = [now(), Date.now()];
    const response = await fetch(`${entry.url}/auth/browser/token`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ verifier }),
      credentials: "omit",
      redirect: "error",
      cache: "no-store",
      signal: AbortSignal.any([signal, AbortSignal.timeout(EXCHANGE_TIMEOUT_MS)]),
    }).catch(() => {
      signal.throwIfAborted();
      return null;
    });
    if (response?.status === 200) {
      let answer: { token: string; remainingMs: number; maximumLifetimeMs: number };
      try {
        answer = grantDocument(await readJSON(response));
      } catch (cause) {
        signal.throwIfAborted();
        throw new Error("Invalid measurement grant", { cause });
      }
      signal.throwIfAborted();
      if (!answer.remainingMs || answer.remainingMs > MAX_GRANT_MS) throw new Error("Invalid measurement grant");
      const { token, remainingMs, maximumLifetimeMs } = answer;
      return { token, deadline: sentAt + remainingMs, expires: sentWall + remainingMs, maximumLifetimeMs };
    }
    if (response) {
      response.body?.cancel().catch(() => {});
      if (response.status === 429) throw new ApprovalLimitError();
      if (response.status === 410) throw new Error("The approval's answer was lost on the way. Sign in again.");
      if (response.status !== 202 && response.status !== 408 && response.status < 500)
        throw new Error(`Approval exchange returned HTTP ${response.status}`);
    }
    await delay(POLL_MS, signal);
  }
  throw new Error("Approval expired. Sign in again.");
}
