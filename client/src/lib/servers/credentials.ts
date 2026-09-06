import {
  authEnabled,
  authenticatedFetch,
  classifyAuthenticationFailure,
  csrfHeader,
  reportAuthenticationRequired,
} from "../auth";
import { authenticationRequired } from "../request-auth";
import { readJSONResponse } from "../api/decode";
import { allowsServerOrigin, type ServerEntry } from "./catalog";
import type { WtMint } from "../runner/workers/wtToken";

/** Only memory owns a delegated grant. History and saved selection retain identity, never this context. */
export interface ServerCredentials {
  server: ServerEntry;
  kind: "public" | "session" | "grant";
  token?: string;
  expiresAt?: number;
}
export class ServerAuthenticationRequired extends Error {
  constructor(readonly server: ServerEntry) {
    super(`Sign in to ${server.name}`);
  }
}
export function serverCredentials(server: ServerEntry): ServerCredentials {
  return {
    server,
    kind:
      server.id === "self" && server.url === location.origin && authEnabled
        ? "session"
        : "public",
  };
}
export function requestOptions(
  context: ServerCredentials | undefined,
  input: string,
  method = "GET",
): { headers: Record<string, string>; credentials: RequestCredentials } {
  if (
    context &&
    !allowsServerOrigin(
      context.server,
      new URL(input, context.server.url).origin,
    )
  )
    throw new Error("Request destination is outside the selected server");
  if (context?.kind === "grant") {
    if (new URL(input, context.server.url).protocol !== "https:")
      throw new Error("Measurement grants require HTTPS");
    if (!context.token || (context.expiresAt ?? 0) <= Date.now())
      throw new ServerAuthenticationRequired(context.server);
    return {
      headers: { Authorization: `Bearer ${context.token}` },
      credentials: "omit",
    };
  }
  if (context?.kind === "public") return { headers: {}, credentials: "omit" };
  return {
    headers: method === "GET" || method === "HEAD" ? {} : csrfHeader(),
    credentials: authEnabled ? "include" : "same-origin",
  };
}
export async function measurementFetch(
  context: ServerCredentials | undefined,
  input: string,
  init?: RequestInit,
): Promise<Response> {
  if (!context || context.kind === "session")
    return authenticatedFetch(input, init);
  const options = requestOptions(context, input, init?.method);
  const response = await fetch(input, {
    ...init,
    ...options,
    headers: {
      ...Object.fromEntries(new Headers(init?.headers)),
      ...options.headers,
    },
    redirect: "error",
  });
  if (!init?.signal?.aborted && authenticationRequired(response)) {
    await response.body?.cancel().catch(() => {});
    throw new ServerAuthenticationRequired(context.server);
  }
  return response;
}
export function socketMint(
  context: ServerCredentials | undefined,
  origin: string,
  path: string,
  kind: "ws" | "wt",
): WtMint | undefined {
  const protectedServer = context ? context.kind !== "public" : authEnabled;
  if (!protectedServer || (kind === "ws" && context?.kind !== "grant"))
    return undefined;
  const url = `${origin}/${kind}/session?target=${encodeURIComponent(origin + path)}`;
  return { url, ...requestOptions(context, url, "POST") };
}
export function reportServerAuthentication(context?: ServerCredentials): void {
  if (!context || context.kind === "session") reportAuthenticationRequired();
  // Remote transport failures are reported to their participant's host, which owns cancellation.
}
export async function classifyServerAuthentication(
  context?: ServerCredentials,
  signal?: AbortSignal,
): Promise<boolean> {
  return !context || context.kind === "session"
    ? classifyAuthenticationFailure(signal)
    : false;
}
function base64url(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replaceAll("=", "");
}
export async function browserApproval(server: ServerEntry): Promise<{
  url: string;
  poll: (signal: AbortSignal) => Promise<ServerCredentials>;
}> {
  if (location.protocol !== "https:" || !server.url.startsWith("https://"))
    throw new Error(
      "Open this interface over HTTPS to authorize a remote server",
    );
  const verifier = base64url(crypto.getRandomValues(new Uint8Array(32)));
  const challenge = base64url(
    new Uint8Array(
      await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)),
    ),
  );
  const url = `${server.url}/auth/browser?${new URLSearchParams({ challenge, client_origin: location.origin })}`;
  return {
    url,
    async poll(signal) {
      const deadline = Date.now() + 120_000;
      while (Date.now() < deadline) {
        signal.throwIfAborted();
        const response = await fetch(`${server.url}/auth/browser/token`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ verifier }),
          credentials: "omit",
          redirect: "error",
          signal: AbortSignal.any([signal, AbortSignal.timeout(5000)]),
        });
        if (response.status === 200) {
          const value = (await readJSONResponse(response)) as Record<
            string,
            unknown
          >;
          signal.throwIfAborted();
          if (
            typeof value.token !== "string" ||
            !/^[A-Za-z0-9_-]{43}$/.test(value.token) ||
            typeof value.remainingMs !== "number" ||
            !Number.isFinite(value.remainingMs) ||
            value.remainingMs <= 0 ||
            value.remainingMs > 8 * 3600_000
          )
            throw new Error("Invalid measurement grant");
          return {
            server,
            kind: "grant",
            token: value.token,
            expiresAt: Date.now() + value.remainingMs,
          };
        }
        await response.body?.cancel().catch(() => {});
        if (response.status !== 202)
          throw new Error(`Approval exchange returned HTTP ${response.status}`);
        signal.throwIfAborted();
        await new Promise<void>((resolve, reject) => {
          const aborted = () => {
            clearTimeout(timer);
            reject(signal.reason);
          };
          const timer = setTimeout(() => {
            signal.removeEventListener("abort", aborted);
            resolve();
          }, 1000);
          signal.addEventListener("abort", aborted, { once: true });
        });
      }
      throw new Error("Approval expired. Sign in again.");
    },
  };
}
