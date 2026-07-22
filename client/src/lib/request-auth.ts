export function redirectForCredentials(
  credentials: RequestCredentials | undefined,
): RequestRedirect | undefined {
  return credentials === "include" ? "error" : undefined;
}
