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

/** Navigate to the login page. Named for the side effect: this replaces the
 *  current document, so nothing after the call in the same task runs. The
 *  `expired` reason is the phrasing key the server-rendered login page uses. */
export function redirectToLogin(): void {
  if (!authEnabled || redirecting) return;
  redirecting = true;
  window.dispatchEvent(new Event("graphite-meter-auth-required"));
  location.replace("/login?reason=expired");
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
  // Concurrent failures (parallel transfer workers) must share one probe, or a
  // single expiry fans out into a burst of /auth/session requests.
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
