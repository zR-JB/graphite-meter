import { afterEach, expect, jest, test } from "bun:test";
import {
  mintWtToken,
  SESSION_REVOKED,
  SESSION_TIMEOUTS,
  SOCKET_REVOKED,
  spendWtToken,
} from "./wtToken";
import { ESTABLISH_BUDGET_MS, LANE_RESTART_BACKOFF_MS } from "../real/budgets";
import { readPin, stubGlobals } from "../../test-helpers.testutil";

const MINT = { url: "https://meter.test/wt/session" };

let restore = () => {};
afterEach(() => {
  restore();
  jest.useRealTimers();
});
const serve = (
  handler: (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>,
) => void (restore = stubGlobals({ fetch: handler }));

test.each([
  [
    "a mint refusal carrying the marker reports the session as gone",
    new Response("no", {
      status: 403,
      headers: { "Graphite-Meter-Auth": "required" },
    }),
    { token: "", authRequired: true },
  ],
  [
    "a bare refusal is a retry, not a login",
    new Response("no", { status: 403 }),
    { token: "", authRequired: false },
  ],
  [
    "a minted token comes back with no auth verdict",
    Response.json({ token: "gmw_abc", expires: 0 }),
    { token: "gmw_abc", authRequired: false },
  ],
  [
    "no mint configured means authentication is off",
    null,
    { token: "", authRequired: false },
  ],
] as const)("%s", async (_name, response, expected) => {
  serve(async () => response!);
  expect(await mintWtToken(response ? MINT : undefined)).toEqual(expected);
});

test("a credentialed mint is an uncached POST with caller headers and no redirects", async () => {
  let init: RequestInit | undefined;
  serve(async (_input, got) => {
    init = got;
    return Response.json({ token: "gmw_abc", expires: 0 });
  });
  await mintWtToken({
    ...MINT,
    credentials: "include",
    headers: { "X-CSRF-Token": "csrf-token" },
  });
  expect(init).toMatchObject({
    method: "POST",
    cache: "no-store",
    redirect: "error",
    credentials: "include",
    headers: { "X-CSRF-Token": "csrf-token" },
  });
});

function respondOnAbort(): () => AbortSignal | undefined {
  let seen: AbortSignal | undefined;
  serve(
    (_input, got) =>
      new Promise<Response>((_, reject) => {
        seen = got?.signal ?? undefined;
        seen?.addEventListener("abort", () => reject(new Error("aborted")));
      }),
  );
  return () => seen;
}

test("a mint that never answers is abandoned on its own bound", async () => {
  respondOnAbort();
  jest.useFakeTimers();
  const pending = mintWtToken({ url: "https://meter.test/hangs" });
  jest.advanceTimersByTime(3_000);
  expect(await pending).toEqual({ token: "", authRequired: false });
});

test("a caller's signal cuts the mint short", async () => {
  const seen = respondOnAbort();
  const caller = new AbortController();
  const pending = mintWtToken(
    { url: "https://meter.test/hangs-too" },
    caller.signal,
  );
  expect(seen()).toBeInstanceOf(AbortSignal);
  expect(seen()).not.toBe(caller.signal);
  caller.abort();
  expect(await pending).toEqual({ token: "", authRequired: false });
});

function countingMint(): () => number {
  let calls = 0;
  serve(async () => {
    calls++;
    return Response.json({ token: "gmw_live", expires: Date.now() + 30_000 });
  });
  return () => calls;
}

test("a re-dial reuses the token the failed dial never spent", async () => {
  const url = "https://meter.test/reused";
  const calls = countingMint();
  const first = await mintWtToken({ url });
  expect(first.token).toBe("gmw_live");
  for (let i = 0; i < 2; i++)
    expect((await mintWtToken({ url })).token).toBe("gmw_live");
  expect(calls()).toBe(1);
  await mintWtToken({ url });
  expect(calls()).toBe(2);
});

test("the reuse window expires after two establish budgets and a retry", async () => {
  const url = "https://meter.test/window";
  jest.useFakeTimers();
  const calls = countingMint();
  expect((await mintWtToken({ url })).token).toBe("gmw_live");
  jest.setSystemTime(
    Date.now() + 2 * ESTABLISH_BUDGET_MS + LANE_RESTART_BACKOFF_MS + 10,
  );
  expect((await mintWtToken({ url })).token).toBe("gmw_live");
  expect(calls()).toBe(2);
});

test("a spent token is never handed out again", async () => {
  const url = "https://meter.test/spent";
  const calls = countingMint();
  const first = await mintWtToken({ url });
  spendWtToken(first.token);
  expect((await mintWtToken({ url })).token).toBe("gmw_live");
  expect(calls()).toBe(2);
});

test("repeated unavailable dials stay within the eight-ticket pool and honor server expiry", async () => {
  jest.useFakeTimers();
  const parked: number[] = [];
  let mints = 0;
  let peak = 0;
  let lifetimeMs = 30_000;
  serve(async () => {
    const now = Date.now();
    for (let i = parked.length - 1; i >= 0; i--)
      if (parked[i] <= now) parked.splice(i, 1);
    if (parked.length >= 8) return new Response(null, { status: 429 });
    parked.push(now + lifetimeMs);
    peak = Math.max(peak, parked.length);
    return Response.json({
      token: `gmw_${++mints}`,
      expires: now + lifetimeMs,
    });
  });
  const mint = { url: "https://meter.test/unavailable" };
  let backoff = 0;
  for (let elapsed = 0; elapsed < 90_000;) {
    expect((await mintWtToken(mint)).token).not.toBe("");
    backoff = backoff ? Math.min(backoff * 2, 2000) : 100;
    elapsed += backoff;
    jest.setSystemTime(Date.now() + backoff);
  }
  expect(peak).toBeLessThanOrEqual(8);
  expect(mints).toBeGreaterThan(8);

  parked.length = 0;
  lifetimeMs = 1000;
  const short = { url: "https://meter.test/short-lifetime" };
  const first = await mintWtToken(short);
  jest.setSystemTime(Date.now() + lifetimeMs);
  expect((await mintWtToken(short)).token).not.toBe(first.token);
});

test("malformed, expiry-free and overflowing mint tokens are refused and never reused", async () => {
  for (const body of [
    "null",
    '{"token":"' + "a".repeat(8193) + '"}',
    '{"token":"gmw_bad","expires":1e999}',
    '{"token":"gmw_bad","expires":null}',
    '{"token":"gmw_legacy"}',
  ]) {
    let calls = 0;
    serve(async () => (calls++, new Response(body)));
    const mint = { url: "https://meter.test/invalid" };
    for (let i = 0; i < 2; i++)
      expect(await mintWtToken(mint), body).toEqual({
        token: "",
        authRequired: false,
      });
    expect(calls).toBe(2);
    restore();
  }
});

test("the browser reads the revoked and timeout lane endings as pinned", async () => {
  const rows = await readPin("laneendings.txt");
  const [, socket, session, reason] = rows.find(
    ([name]) => name === "revoked",
  )!;
  expect(SOCKET_REVOKED).toEqual({ code: Number(socket), reason });
  expect(SESSION_REVOKED).toBe(Number(session));
  expect(SESSION_TIMEOUTS).toEqual(
    rows
      .filter(([name]) => name === "idle" || name === "lifetime")
      .map(([, , code]) => Number(code)),
  );
});
