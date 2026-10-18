// The page's own facts and its sign-in, which adds cookies and CSRF to its requests.
import { signal, type Read } from "../../lib/signal";
import { readJSON, release, session as sessionDocument, signInMarker } from "../../protocol/documents";
import { AUTH_ROUTES } from "../../protocol/routes";

export interface PageConfig {
  origin: string;
  secure: boolean;
  /** The server marks authenticated pages. The document stays the only owner of that fact. */
  auth: boolean;
  /** The operator's default for saving results. The user's own choice wins. */
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

type SessionEnd = "expired" | "renew";

/** The sign-in cannot cover a planned test. The message says what the user can do. */
export class CoverageError extends Error {}

export interface Account {
  name: string;
  provider: string;
  csrf: string;
}

export interface PageSession {
  /** Set once when the page's sign-in ends. The app then saves what it can and navigates to sign-in. */
  readonly ended: Read<SessionEnd | null>;
  end(reason: SessionEnd): void;
  /** Adds the sign-in's CSRF token, which its readable cookie mirrors, to requests that change state. */
  headers(method: string): Record<string, string>;
  /** Asks the page's own server, with its cookies, CSRF on unsafe methods, and no redirect with credentials. */
  fetch(input: string, init?: RequestInit): Promise<Response>;
  /** Asks whether the sign-in has ended, from one shared check. A yes ends the session. */
  expired(signal?: AbortSignal): Promise<boolean>;
  /** Gives the sign-in's monotonic deadline when it covers `requiredMs`, or null without authentication. */
  coverage(requiredMs: number, signal: AbortSignal): Promise<number | null>;
  /** Names who is signed in, for the account control, or null without authentication or when it cannot be read. */
  account(signal: AbortSignal): Promise<Account | null>;
}

/** One check whether a sign-in or grant ended gets this long. */
export const ENDED_CHECK_MS = 3_000;
const SAFE_METHODS = new Set(["GET", "HEAD", "OPTIONS"]);

/** Asks whether a sign-in or grant ended, from one `check` in flight for everyone who asks, so an expiry never fans
    out into a burst from parallel lanes. An asker that gave up hears no. A failed check is no evidence. */
export function endedCheck(check: () => Promise<Response>): (signal?: AbortSignal) => Promise<boolean> {
  let checking: Promise<boolean> | null = null;
  const marked = () =>
    check().then(
      (response) => (release(response), signInMarker(response)),
      () => false,
    );
  return async (signal) => {
    if (signal?.aborted) return false;
    checking ??= marked().finally(() => (checking = null));
    return (await checking) && !signal?.aborted;
  };
}

/** Puts the sign-in's CSRF token, mirrored in a readable cookie, in a header for methods that change state. */
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

  const end = (reason: SessionEnd) => {
    if (config.auth && !ended.peek()) ended.set(reason);
  };
  const loginHeaders = (method: string) => (config.auth ? csrfHeaders(method, document.cookie) : {});
  async function request(input: string, init: RequestInit = {}): Promise<Response> {
    const headers = new Headers(init.headers);
    for (const [name, value] of Object.entries(loginHeaders(init.method ?? "GET"))) headers.set(name, value);
    const credentials = config.auth ? "include" : init.credentials;
    // A hop that redirects a credentialed request to a sign-in page must not pass for its answer.
    const redirect = credentials === "include" || credentials === "omit" ? "error" : init.redirect;
    const response = await fetch(input, { ...init, headers, credentials, redirect });
    if (!init.signal?.aborted && signInMarker(response)) end("expired");
    return response;
  }
  const sessionRequest = (signal?: AbortSignal) => {
    const timeout = AbortSignal.timeout(ENDED_CHECK_MS);
    return request(AUTH_ROUTES.session, {
      cache: "no-store",
      signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
    });
  };
  async function login(signal: AbortSignal) {
    const response = await sessionRequest(signal);
    if (!response.ok) {
      release(response);
      throw new Error(`${AUTH_ROUTES.session} answered HTTP ${response.status}`);
    }
    return sessionDocument(await readJSON(response));
  }
  const signedOut = endedCheck(sessionRequest);

  return {
    ended,
    end,
    headers: loginHeaders,
    fetch: request,
    async expired(signal) {
      if (!config.auth || signal?.aborted) return false;
      return !!ended.peek() || signedOut(signal);
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
        throw new CoverageError("Could not verify the sign-in.", { cause });
      }
      signal.throwIfAborted();
      if (requiredMs > lifetime.maximumLifetimeMs)
        throw new CoverageError("This test is longer than a sign-in can last. Shorten it before starting.");
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
