import {
  redirectForCredentials,
  sessionAuthenticationRequired,
} from "./request-auth";

let redirecting = false;
let classifying: Promise<boolean> | null = null;

export const authEnabled =
  typeof document !== "undefined" &&
  document
    .querySelector('meta[name="graphite-meter-auth"]')
    ?.getAttribute("content") === "enabled";

export function requireAuthentication(): void {
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
  const pending = (classifying ??= sessionAuthenticationRequired(
    location.origin,
  ));
  try {
    const required = await pending;
    if (required) requireAuthentication();
    return required;
  } finally {
    if (classifying === pending) classifying = null;
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
  if (
    response.status === 403 &&
    response.headers.get("Graphite-Meter-Auth") === "required"
  ) {
    requireAuthentication();
  }
  return response;
}
