import "./state/runes.testutil";
import { expect, jest, test } from "bun:test";
import * as auth from "./auth";
import { stubGlobals } from "./test-helpers.testutil";
import {
  TEST_BUILD_TOKENS,
  testServerCatalog,
  testServerDiscovery,
} from "./runner/test-helpers.testutil";

// The server marks authenticated pages; every stubbed document carries it.
const authenticatedPage = () => ({ getAttribute: () => "enabled" });

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
      querySelector: authenticatedPage,
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

test("a caller abort mid-flight rejects coverage without a renew report", async () => {
  let request: AbortSignal | undefined;
  const env = environment((async (
    _input: RequestInfo | URL,
    init?: RequestInit,
  ) => {
    request = init!.signal!;
    // The response is already on its way when the caller gives up.
    await new Promise((resolve) => request!.addEventListener("abort", resolve));
    return Response.json({ remainingMs: 100, maximumLifetimeMs: 10_000 });
  }) as unknown as typeof fetch);
  try {
    const caller = new AbortController();
    const checking = auth.requireSessionCoverage(1000, caller.signal);
    caller.abort();
    await expect(checking).rejects.toMatchObject({ name: "AbortError" });
    expect(request!.aborted).toBe(true);
    expect(env.reported).toEqual([]);
  } finally {
    env.restore();
  }
});

test("coverage gives up on a silent server after 3 s and leaves no timer after success", async () => {
  let respond = false;
  const requests: AbortSignal[] = [];
  const env = environment((async (
    _input: RequestInfo | URL,
    init?: RequestInit,
  ) => {
    const signal = init!.signal!;
    requests.push(signal);
    if (respond)
      return Response.json({ remainingMs: 10_000, maximumLifetimeMs: 10_000 });
    return new Promise<Response>((_resolve, reject) =>
      signal.addEventListener("abort", () => reject(signal.reason)),
    );
  }) as unknown as typeof fetch);
  jest.useFakeTimers();
  try {
    const silent = auth.requireSessionCoverage(1000);
    jest.advanceTimersByTime(2_999);
    expect(requests[0].aborted).toBe(false);
    jest.advanceTimersByTime(1);
    expect(requests[0].aborted).toBe(true);
    await expect(silent).rejects.toBeInstanceOf(auth.SessionCoverageError);
    respond = true;
    expect(await auth.requireSessionCoverage(1000)).toMatchObject({
      remainingMs: 10_000,
    });
    jest.advanceTimersByTime(3_000);
    expect(requests[1].aborted).toBe(false);
  } finally {
    jest.useRealTimers();
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
    await import("./runner/controller.svelte");
  const { store } = await import("./state/store.svelte");
  let started!: (signal: AbortSignal) => void;
  const preparing = new Promise<AbortSignal>((resolve) => (started = resolve));
  const engine = createApplicationController(store, {
    loadCatalog: testServerCatalog,
    discover: testServerDiscovery,
    prepare: async (_config, _previous, _roles, signal) => {
      started(signal);
      return new Promise((_resolve, reject) =>
        signal.addEventListener("abort", () =>
          reject(new DOMException("Aborted", "AbortError")),
        ),
      );
    },
  });
  try {
    const boot = engine.boot();
    const signal = await preparing;
    await auth.authenticatedFetch("/auth/session");
    expect(signal.aborted).toBe(true);
    expect(env.navigations).toEqual(["/login?reason=expired"]);
    auth.reportAuthenticationRequired();
    expect(env.navigations).toHaveLength(1);
    await boot;
  } finally {
    engine.dispose();
    env.restore();
  }
});
