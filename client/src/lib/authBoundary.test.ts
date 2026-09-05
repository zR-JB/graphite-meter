import "./state/runes.test";
import { beforeAll, expect, test } from "bun:test";
import { stubGlobals } from "./test-helpers.test";
import { TEST_BUILD_TOKENS } from "./runner/test-helpers.test";

// Match the server's authenticated-page marker before importing auth policy.
let auth: typeof import("./auth");
beforeAll(async () => {
  const restoreMarker = stubGlobals({
    document: { querySelector: () => ({ getAttribute: () => "enabled" }) },
  });
  try {
    auth = await import("./auth");
  } finally {
    restoreMarker();
  }
});

function environment(request: typeof fetch) {
  const target = new EventTarget();
  const reported: string[] = [];
  target.addEventListener(auth.AUTHENTICATION_REQUIRED_EVENT, (event) => {
    reported.push((event as CustomEvent<string>).detail);
  });
  const navigations: string[] = [];
  const restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    window: target,
    document: Object.assign(new EventTarget(), {
      cookie: "__Host-gm_csrf=csrf",
      visibilityState: "visible",
    }),
    navigator: { onLine: true },
    location: {
      origin: "https://meter.test",
      replace: (url: string) => navigations.push(url),
    },
    fetch: request,
  });
  return { reported, navigations, restore };
}

const responseFetch = (respond: () => Response) =>
  (async () => respond()) as unknown as typeof fetch;

test("session coverage cancels an oversized stream without authorizing or navigating", async () => {
  let canceled = false;
  let reads = 0;
  const env = environment(
    responseFetch(
      () =>
        new Response(
          new ReadableStream<Uint8Array>({
            pull(controller) {
              reads++;
              controller.enqueue(new Uint8Array(16_384));
            },
            cancel() {
              canceled = true;
            },
          }),
        ),
    ),
  );
  try {
    await expect(auth.requireSessionCoverage(1000)).rejects.toBeInstanceOf(
      auth.SessionCoverageError,
    );
    expect(canceled).toBe(true);
    expect(reads).toBeLessThanOrEqual(6);
    expect(env.reported).toEqual([]);
    expect(env.navigations).toEqual([]);
  } finally {
    env.restore();
  }
});

test("coverage reports renewal while malformed lifetimes cannot trigger login", async () => {
  let response = Response.json({ remainingMs: 100, maximumLifetimeMs: 10_000 });
  const env = environment(responseFetch(() => response));
  try {
    await expect(auth.requireSessionCoverage(1000)).rejects.toThrow(
      "Sign in again",
    );
    expect(env.reported).toEqual(["renew"]);
    for (const body of [
      "null",
      "[]",
      '{"remainingMs":100,"maximumLifetimeMs":1e999}',
    ]) {
      response = new Response(body);
      await expect(auth.requireSessionCoverage(1000)).rejects.toBeInstanceOf(
        auth.SessionCoverageError,
      );
    }
    expect(env.reported).toEqual(["renew"]);
    expect(env.navigations).toEqual([]);
  } finally {
    env.restore();
  }
});

test("transport auth reports preserve marker, credential and cancellation boundaries", async () => {
  let marker = false;
  let request: RequestInit | undefined;
  const env = environment((async (
    _input: RequestInfo | URL,
    init?: RequestInit,
  ) => {
    request = init;
    return new Response(null, {
      status: 403,
      headers: marker ? { "Graphite-Meter-Auth": "required" } : {},
    });
  }) as unknown as typeof fetch);
  try {
    await auth.authenticatedFetch("https://meter.test/upload/session", {
      method: "POST",
    });
    expect(request).toMatchObject({
      credentials: "include",
      redirect: "error",
    });
    expect(new Headers(request!.headers).get("X-CSRF-Token")).toBe("csrf");
    expect(env.reported).toEqual([]);
    marker = true;
    await auth.authenticatedFetch("/auth/session");
    expect(env.reported).toEqual(["expired"]);
    const canceled = new AbortController();
    canceled.abort();
    await auth.authenticatedFetch("/auth/session", { signal: canceled.signal });
    expect(env.reported).toEqual(["expired"]);
    expect(env.navigations).toEqual([]);
  } finally {
    env.restore();
  }
});

test("the application cancels preparation before navigating once and relinquishes auth ownership", async () => {
  const env = environment(
    responseFetch(
      () =>
        new Response(null, {
          status: 403,
          headers: { "Graphite-Meter-Auth": "required" },
        }),
    ),
  );
  const { createApplicationController } =
    await import("./runner/engine.svelte");
  const { store } = await import("./state/store.svelte");
  let preparing: AbortSignal | null = null;
  const engine = createApplicationController(store, {
    prepare: async (_config, _previous, _roles, signal) => {
      preparing = signal;
      return new Promise((_resolve, reject) =>
        signal.addEventListener("abort", () =>
          reject(new DOMException("Aborted", "AbortError")),
        ),
      );
    },
  });
  try {
    const boot = engine.boot();
    expect(preparing).not.toBeNull();
    await auth.authenticatedFetch("/auth/session");
    expect(preparing!.aborted).toBe(true);
    expect(env.navigations).toEqual(["/login?reason=expired"]);
    auth.reportAuthenticationRequired();
    expect(env.navigations).toHaveLength(1);
    await boot;
  } finally {
    engine.dispose();
    env.restore();
  }
});
