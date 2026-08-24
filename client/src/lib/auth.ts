import {
  redirectForCredentials,
  sessionAuthenticationRequired,
  authenticationRequired,
} from "./request-auth";

let redirecting = false;
let pendingClassification: Promise<boolean> | null = null;

export const authEnabled =
  typeof document !== "undefined" &&
  document
    .querySelector('meta[name="graphite-meter-auth"]')
    ?.getAttribute("content") === "enabled";

/** Replaces the current document, so the calling task stops executing here.
 *  `expired` is the phrasing key the server-rendered login page reads. */
export function redirectToLogin(reason = "expired"): void {
  if (!authEnabled || redirecting) return;
  redirecting = true;
  window.dispatchEvent(new Event("graphite-meter-auth-required"));
  location.replace(`/login?reason=${encodeURIComponent(reason)}`);
}

export class SessionCoverageError extends Error {}

export interface SessionBudget {
  remainingMs: number;
  maximumLifetimeMs: number;
  checkedAt: number;
}

export type SessionCoverage = "enough" | "renew" | "too-long" | "invalid";

export function classifySessionCoverage(
  requiredMs: number,
  remainingMs: unknown,
  maximumLifetimeMs: unknown,
): SessionCoverage {
  if (
    !Number.isFinite(requiredMs) ||
    requiredMs < 0 ||
    typeof remainingMs !== "number" ||
    !Number.isFinite(remainingMs) ||
    remainingMs < 0 ||
    typeof maximumLifetimeMs !== "number" ||
    !Number.isFinite(maximumLifetimeMs) ||
    maximumLifetimeMs <= 0 ||
    remainingMs > maximumLifetimeMs
  )
    return "invalid";
  if (requiredMs > maximumLifetimeMs) return "too-long";
  return requiredMs > remainingMs ? "renew" : "enough";
}

export function sessionBudgetCovers(
  budget: SessionBudget,
  requiredMs: number,
  now = performance.now(),
): boolean {
  return requiredMs <= budget.remainingMs - (now - budget.checkedAt);
}

export function liveScheduleFitsSession(
  budget: SessionBudget | null,
  activeMs: number,
  candidateMs: number,
  marginMs: number,
  now = performance.now(),
): boolean {
  return (
    candidateMs <= activeMs ||
    budget === null ||
    sessionBudgetCovers(budget, candidateMs + marginMs, now)
  );
}

export async function requireSessionCoverage(
  requiredMs: number,
  localSignal?: AbortSignal,
): Promise<SessionBudget | null> {
  if (localSignal?.aborted) throw new DOMException("Aborted", "AbortError");
  if (!authEnabled) return null;
  const controller = new AbortController();
  const relayAbort = () => controller.abort();
  localSignal?.addEventListener("abort", relayAbort, { once: true });
  const timeout = setTimeout(() => controller.abort(), 3000);
  try {
    const response = await authenticatedFetch("/auth/session", {
      cache: "no-store",
      signal: controller.signal,
    });
    if (!response.ok)
      throw new SessionCoverageError("Could not verify the session lifetime.");
    const body = (await response.json()) as Record<string, unknown>;
    const remainingMs = body.remainingMs;
    const maximumLifetimeMs = body.maximumLifetimeMs;
    const coverage = classifySessionCoverage(
      requiredMs,
      remainingMs,
      maximumLifetimeMs,
    );
    if (coverage === "invalid")
      throw new SessionCoverageError("Could not verify the session lifetime.");
    if (coverage === "too-long")
      throw new SessionCoverageError(
        "This test is longer than the maximum session lifetime. Shorten it before starting.",
      );
    if (coverage === "renew") {
      redirectToLogin("renew");
      throw new SessionCoverageError(
        "Sign in again before starting this long test.",
      );
    }
    return {
      remainingMs: remainingMs as number,
      maximumLifetimeMs: maximumLifetimeMs as number,
      checkedAt: performance.now(),
    };
  } catch (cause) {
    if (localSignal?.aborted) throw new DOMException("Aborted", "AbortError");
    if (cause instanceof SessionCoverageError) throw cause;
    throw new SessionCoverageError("Could not verify the session lifetime.", {
      cause,
    });
  } finally {
    clearTimeout(timeout);
    localSignal?.removeEventListener("abort", relayAbort);
  }
}

export function csrfHeader(): Record<string, string> {
  if (!authEnabled || typeof document === "undefined") return {};
  const prefix = "__Host-gm_csrf=";
  const value = document.cookie
    .split("; ")
    .find((entry) => entry.startsWith(prefix))
    ?.slice(prefix.length);
  return value ? { "X-CSRF-Token": decodeURIComponent(value) } : {};
}

export async function classifyAuthenticationFailure(
  localSignal?: AbortSignal,
): Promise<boolean> {
  if (!authEnabled || localSignal?.aborted) return false;
  // Parallel transfer workers share one probe: a single expiry must not fan
  // out into a burst of /auth/session requests.
  const pending = (pendingClassification ??= sessionAuthenticationRequired(
    location.origin,
  ));
  try {
    const required = await pending;
    if (required) redirectToLogin();
    return required;
  } finally {
    if (pendingClassification === pending) pendingClassification = null;
  }
}

export async function authenticatedFetch(
  input: RequestInfo | URL,
  init?: RequestInit,
): Promise<Response> {
  const headers = new Headers(init?.headers);
  const method = (init?.method ?? "GET").toUpperCase();
  if (method !== "GET" && method !== "HEAD" && method !== "OPTIONS") {
    for (const [name, value] of Object.entries(csrfHeader()))
      headers.set(name, value);
  }

  const credentials = authEnabled ? "include" : init?.credentials;
  const response = await fetch(input, {
    ...init,
    headers,
    credentials,
    redirect: redirectForCredentials(credentials),
  });
  if (authenticationRequired(response)) {
    redirectToLogin();
  }
  return response;
}
