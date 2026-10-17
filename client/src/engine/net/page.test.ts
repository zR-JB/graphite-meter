import { afterEach, expect, test } from "bun:test";
import { fakeNet, MARKER, page as publicPage, type Answer } from "./fakes.testutil";
import { CoverageError, createPageSession } from "./page";

const page = { ...publicPage, auth: true };
let net: ReturnType<typeof fakeNet> | undefined;
afterEach(() => {
  net?.restore();
  Reflect.deleteProperty(globalThis, "document");
});

/** Signs in on the fake network, with cookies holding the CSRF token. */
function signedIn(answer: () => Answer, config = page) {
  net?.restore();
  net = fakeNet(answer);
  Object.assign(globalThis, { document: { cookie: "theme=dark; __Host-gm_csrf=a%2Bb" } });
  return createPageSession(config, net.now);
}

const lifetime = (remainingMs: number, maximumLifetimeMs = 3_600_000): Answer => ({
  body: { name: "Ada", provider: "password", csrf: "a+b", remainingMs, maximumLifetimeMs },
  afterMs: 100,
});

test("a login must cover the planned test: renew when it ends too soon, refuse what it cannot last", async () => {
  const unreadable = "Could not verify the sign-in.";
  for (const [answer, outcome, ended] of [
    [lifetime(60_000), 60_000, null],
    [lifetime(59_999), "Sign in again before starting this long test.", "renew"],
    [lifetime(59_999, 59_999), "This test is longer than a sign-in can last. Shorten it before starting.", null],
    [{ status: 500 }, unreadable, null],
    [
      { body: { name: "Ada", provider: "password", csrf: "a+b", remainingMs: 2, maximumLifetimeMs: 1 } },
      unreadable,
      null,
    ],
    ["hang", unreadable, null],
    [{ status: 403, headers: MARKER }, unreadable, "expired"],
  ] as const) {
    const session = signedIn(() => answer);
    const covered = session
      .coverage(60_000, new AbortController().signal)
      .catch((error: CoverageError) => error.message);
    await net!.advance(5_000);
    expect({ outcome: await covered, ended: session.ended.peek() }, JSON.stringify(answer)).toEqual({ outcome, ended });
  }
  // A start cancelled while its coverage is read ends as an abort, never as a renewal.
  const session = signedIn(() => lifetime(1));
  const cancel = new AbortController();
  const covered = session.coverage(60_000, cancel.signal).catch((error: Error) => error.name);
  await net!.advance(50);
  cancel.abort();
  expect({ outcome: await covered, ended: session.ended.peek() }).toEqual({ outcome: "AbortError", ended: null });
  // Without authentication there is nothing to cover and nothing is asked.
  const open = signedIn(() => "unreachable", publicPage);
  expect(await open.coverage(60_000, new AbortController().signal)).toBeNull();
  expect(net!.requests).toEqual([]);
});

test("one /auth/session check answers every lane asking whether the login ended; only its marker ends it", async () => {
  for (const [answer, expired] of [
    ["unreachable", false],
    [{ status: 403, afterMs: 100 }, false],
    [{ status: 403, headers: MARKER, afterMs: 100 }, true],
  ] as const) {
    const session = signedIn(() => answer);
    const asked = Promise.all([session.expired(), session.expired(), session.expired()]);
    await net!.advance(200);
    expect(await asked, JSON.stringify(answer)).toEqual([expired, expired, expired]);
    expect(net!.requests.length).toBe(1);
    expect(session.ended.peek()).toBe(expired ? "expired" : null);
  }
  // The page's own requests carry its cookies, and its CSRF token where they change state. A marker ends the login.
  const session = signedIn(() => ({ status: 403, headers: MARKER }));
  expect((await session.fetch("/upload/session", { method: "POST" })).status).toBe(403);
  const { init } = net!.requests[0]!;
  expect({ ...init, headers: Object.fromEntries(new Headers(init.headers)) }).toEqual({
    method: "POST",
    headers: { "x-csrf-token": "a+b" },
    credentials: "include",
    redirect: "error",
  });
  expect(session.ended.peek()).toBe("expired");
});
