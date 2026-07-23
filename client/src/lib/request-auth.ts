export function redirectForCredentials(
  credentials: RequestCredentials | undefined,
): RequestRedirect | undefined {
  return credentials === "include" ? "error" : undefined;
}

/** Whether a response is the boundary saying "authenticate", as opposed to
 *  anything else that answers 403. Only the marker means an expired session;
 *  a bare 403 from a proxy or WAF in front of the server must not be read as
 *  one, or a misconfigured hop turns into a login redirect loop. */
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
    return false;
  } finally {
    clearTimeout(timeout);
  }
}
