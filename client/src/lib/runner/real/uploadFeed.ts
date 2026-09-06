import {
  authenticationRequired,
  redirectForCredentials,
  sessionAuthenticationRequired,
} from "../../request-auth";
import { nextBackoff } from "../workers/backoff";
import { readProgressFeed, type ProgressEvent } from "../workers/progressFeed";
import { classifyUploadFailure } from "../uploadFailure";

export function startUploadFeed(options: {
  url: string;
  csrf: Record<string, string>;
  credentials: RequestCredentials;
  onEvent: (
    event:
      | ProgressEvent
      | { type: "stall"; detail: string }
      | { type: "auth-required" },
  ) => void;
}): { finalize(): void; dispose(): void } {
  const { url, csrf, credentials, onEvent } = options;
  const controller = new AbortController();
  const { signal } = controller;
  const feed = { lastN: 0, lastT: 0 };
  let finishing = false;
  let backoff = 0;
  let wakeReconnect: (() => void) | undefined;

  function dispose(): void {
    controller.abort();
    wakeReconnect?.();
  }

  function emit(event: Parameters<typeof onEvent>[0]): void {
    if (signal.aborted) return;
    onEvent(event);
    if (
      event.type === "complete" ||
      event.type === "fatal" ||
      event.type === "auth-required"
    )
      dispose();
  }

  async function expiredSession(): Promise<boolean> {
    if (signal.aborted || credentials !== "include") return false;
    const expired = await sessionAuthenticationRequired(
      location.origin,
      signal,
      (input, init) =>
        fetch(input, {
          ...init,
          signal: AbortSignal.any([
            signal,
            ...(init?.signal ? [init.signal] : []),
          ]),
        }),
    );
    if (signal.aborted) return false;
    if (expired) emit({ type: "auth-required" });
    return expired;
  }

  function delay(ms: number): Promise<void> {
    return new Promise((resolve) => {
      const timer = setTimeout(wake, ms);
      function wake(): void {
        clearTimeout(timer);
        wakeReconnect = undefined;
        resolve();
      }
      wakeReconnect = wake;
      if (signal.aborted) wake();
    });
  }

  async function run(): Promise<void> {
    while (!signal.aborted) {
      let detail = "progress stream closed";
      try {
        const response = await fetch(url, {
          cache: "no-store",
          headers: {
            ...(csrf.Authorization
              ? { Authorization: csrf.Authorization }
              : {}),
            accept: "application/x-ndjson",
          },
          signal,
          credentials,
          redirect: redirectForCredentials(credentials),
        });
        if (signal.aborted) {
          void response.body?.cancel().catch(() => {});
          return;
        }
        if (!response.ok) {
          void response.body?.cancel().catch(() => {});
          if (signal.aborted) return;
          if (authenticationRequired(response)) {
            emit({ type: "auth-required" });
            return;
          }
          if (
            (response.status >= 400 && response.status < 500) ||
            response.status === 503
          ) {
            emit({
              type: "fatal",
              detail: `progress returned HTTP ${response.status}`,
              cause: classifyUploadFailure(
                response.status,
                response.headers.get("X-Graphite-Upload-Refusal"),
              ),
            });
            return;
          }
          throw new Error(`progress returned HTTP ${response.status}`);
        }
        if (!response.body)
          throw new Error(`progress returned HTTP ${response.status}`);
        await readProgressFeed(response.body, feed, (event) => {
          if (event.type === "open") backoff = 0;
          emit(event);
        });
      } catch (error) {
        if (signal.aborted || (await expiredSession())) return;
        detail = String(error);
      }
      if (signal.aborted) return;
      emit({ type: "stall", detail });
      backoff = nextBackoff(backoff, 100, 2000);
      await delay(backoff);
    }
  }

  async function finish(): Promise<void> {
    if (signal.aborted || finishing) return;
    finishing = true;
    wakeReconnect?.();
    try {
      const response = await fetch(url, {
        method: "DELETE",
        cache: "no-store",
        headers: csrf,
        signal,
        credentials,
        redirect: redirectForCredentials(credentials),
      });
      void response.body?.cancel().catch(() => {});
      if (signal.aborted) return;
      if (authenticationRequired(response)) emit({ type: "auth-required" });
      else if (!response.ok) dispose();
    } catch {
      if (!signal.aborted) await expiredSession();
      dispose();
    }
  }

  void run();
  return {
    finalize: () => {
      void finish();
    },
    dispose,
  };
}
