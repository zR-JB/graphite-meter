// The page's own facts, read once from its document, and its login: cookies and CSRF on its requests, the lifetime a
// planned test needs, and the one end the app navigates on.
import { signal, type Read } from "../../lib/signal";
import { readJSON, session as sessionDocument } from "../../protocol/documents";

export interface PageConfig {
  origin: string;
  secure: boolean;
  /** The server marks authenticated pages; the document stays the only owner of that fact. */
  auth: boolean;
  /** The operator's default for saving results; the user's own choice wins. */
  historyDefault: boolean;
  webTransport: "available" | "insecure-page" | "no-api";
}

export function readPageConfig(): PageConfig {
  const meta = (name: string) => document.querySelector(`meta[name="${name}"]`)?.getAttribute("content");
  return {
    origin: location.origin,
    secure: location.protocol === "https:",
    auth: meta("graphite-meter-auth") === "enabled",
    historyDefault: meta("graphite-meter-result-history-default") === "true",
    webTransport: typeof WebTransport !== "undefined" ? "available" : isSecureContext ? "no-api" : "insecure-page",
  };
}

/** Only HTTP 403 with this marker means a login or grant has ended; other refusals and failures are not evidence. */
export const signInMarker = (response: Response) =>
  response.status === 403 && response.headers.get("Graphite-Meter-Auth") === "required";

export type SessionEnd = "expired" | "renew";

/** The login cannot cover a planned test; the message says what the user can do. */
export class CoverageError extends Error {}

export interface Account {
  name: string;
  provider: string;
  csrf: string;
}

export interface PageSession {
  /** Set once when the page's login ends; the app saves what it can and navigates to sign-in. */
  readonly ended: Read<SessionEnd | null>;
  end(reason: SessionEnd): void;
  /** What the login adds to a request: the CSRF token its readable cookie mirrors, on methods that change state. */
  headers(method: string): Record<string, string>;
  /** A request to the page's own server: its cookies, CSRF on unsafe methods, and no redirect with credentials. */
  fetch(input: string, init?: RequestInit): Promise<Response>;
  /** Whether the login has ended, from one shared /auth/session check of at most 3 s; a yes ends the session. */
  expired(signal?: AbortSignal): Promise<boolean>;
  /** The login's monotonic deadline when it covers `requiredMs`, null without authentication. */
  coverage(requiredMs: number, signal: AbortSignal): Promise<number | null>;
  /** Who is signed in, for the account control; null without authentication or when it cannot be read. */
  account(signal: AbortSignal): Promise<Account | null>;
}

const SESSION_CHECK_MS = 3_000;
const SAFE_METHODS = new Set(["GET", "HEAD", "OPTIONS"]);

/** The login's CSRF token, which it mirrors into a readable cookie, as a header for methods that change state. */
export function csrfHeaders(method: string, cookie: string): Record<string, string> {
  if (SAFE_METHODS.has(method.toUpperCase())) return {};
  const prefix = "__Host-gm_csrf=";
  const value = cookie
    .split("; ")
    .find((entry) => entry.startsWith(prefix))
    ?.slice(prefix.length);
  try {
    return value ? { "X-CSRF-Token": decodeURIComponent(value) } : {};
  } catch {
    return {};
  }
}

export function createPageSession(config: PageConfig, now: () => number): PageSession {
  const ended = signal<SessionEnd | null>(null);
  // A single expiry must not fan out into a burst of /auth/session requests from parallel lanes.
  let checking: Promise<boolean> | null = null;

  const end = (reason: SessionEnd) => {
    if (config.auth && !ended.peek()) ended.set(reason);
  };
  const loginHeaders = (method: string) => (config.auth ? csrfHeaders(method, document.cookie) : {});
  async function request(input: string, init: RequestInit = {}): Promise<Response> {
    const headers = new Headers(init.headers);
    for (const [name, value] of Object.entries(loginHeaders(init.method ?? "GET"))) headers.set(name, value);
    const credentials = config.auth ? "include" : init.credentials;
    // A hop that redirects a credentialed request to a login page must not pass for its answer.
    const redirect = credentials === "include" || credentials === "omit" ? "error" : init.redirect;
    const response = await fetch(input, { ...init, headers, credentials, redirect });
    if (!init.signal?.aborted && signInMarker(response)) end("expired");
    return response;
  }
  async function login(signal: AbortSignal) {
    const timeout = AbortSignal.any([signal, AbortSignal.timeout(SESSION_CHECK_MS)]);
    const response = await request("/auth/session", { cache: "no-store", signal: timeout });
    if (!response.ok) {
      response.body?.cancel().catch(() => {});
      throw new Error(`/auth/session answered HTTP ${response.status}`);
    }
    return sessionDocument(await readJSON(response));
  }
  async function check(): Promise<boolean> {
    try {
      const response = await fetch("/auth/session", {
        cache: "no-store",
        credentials: "include",
        redirect: "error",
        signal: AbortSignal.timeout(SESSION_CHECK_MS),
      });
      response.body?.cancel().catch(() => {});
      const required = signInMarker(response);
      if (required) end("expired");
      return required;
    } catch {
      return false;
    }
  }

  return {
    ended,
    end,
    headers: loginHeaders,
    fetch: request,
    async expired(signal) {
      if (!config.auth || signal?.aborted) return false;
      if (ended.peek()) return true;
      checking ??= check().finally(() => (checking = null));
      return (await checking) && !signal?.aborted;
    },
    async coverage(requiredMs, signal) {
      signal.throwIfAborted();
      if (!config.auth) return null;
      const sentAt = now();
      let lifetime: { remainingMs: number; maximumLifetimeMs: number };
      try {
        lifetime = await login(signal);
      } catch (cause) {
        signal.throwIfAborted();
        throw new CoverageError("Could not verify the session lifetime.", { cause });
      }
      signal.throwIfAborted();
      if (requiredMs > lifetime.maximumLifetimeMs)
        throw new CoverageError("This test is longer than the maximum session lifetime. Shorten it before starting.");
      if (requiredMs > lifetime.remainingMs) {
        end("renew");
        throw new CoverageError("Sign in again before starting this long test.");
      }
      return sentAt + lifetime.remainingMs;
    },
    async account(signal) {
      if (!config.auth) return null;
      try {
        const { name, provider, csrf: token } = await login(signal);
        return { name, provider, csrf: token };
      } catch {
        return null;
      }
    },
  };
}
