export function redirectForCredentials(
  credentials: RequestCredentials | undefined,
): RequestRedirect | undefined {
  return credentials === "include" ? "error" : undefined;
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
    return (
      response.status === 403 &&
      response.headers.get("Graphite-Meter-Auth") === "required"
    );
  } catch {
    return false;
  } finally {
    clearTimeout(timeout);
  }
}
