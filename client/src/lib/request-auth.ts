export function redirectForCredentials(
  credentials: RequestCredentials | undefined,
): RequestRedirect | undefined {
  return credentials === "include" ? "error" : undefined;
}

/* Only the explicit marker means an expired session. */
export function authenticationRequired(response: {
  status: number;
  headers: Pick<Headers, "get">;
}): boolean {
  return (
    response.status === 403 &&
    response.headers.get("Graphite-Meter-Auth") === "required"
  );
}

type FetchRequest = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

export async function sessionAuthenticationRequired(
  origin: string,
  localSignal?: AbortSignal,
  request: FetchRequest = fetch,
): Promise<boolean> {
  if (localSignal?.aborted) return false;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 3000);
  try {
    const response = await request(new URL("/auth/session", origin), {
      cache: "no-store",
      credentials: "include",
      redirect: "error",
      signal: controller.signal,
    });
    return authenticationRequired(response);
  } catch {
    // Transport failure, refused redirect, and timeout are not expiry evidence.
    return false;
  } finally {
    clearTimeout(timeout);
  }
}
