import { test, expect } from "bun:test";
import { mintWtToken, withWtToken } from "./wtToken";

const MINT = { url: "https://meter.test/wt/session" };

function respondWith(response: Response): () => void {
  const real = globalThis.fetch;
  globalThis.fetch = (async () => response) as unknown as typeof fetch;
  return () => {
    globalThis.fetch = real;
  };
}

test("a mint refusal carrying the marker reports the session as gone", async () => {
  const restore = respondWith(
    new Response("no", {
      status: 403,
      headers: { "Graphite-Meter-Auth": "required" },
    }),
  );
  try {
    expect(await mintWtToken(MINT)).toEqual({ token: "", authRequired: true });
  } finally {
    restore();
  }
});

// A proxy or WAF answering 403 is not evidence of expiry, so the caller retries
// rather than bouncing the page to a login it did not need.
test("a bare refusal is a retry, not a login", async () => {
  const restore = respondWith(new Response("no", { status: 403 }));
  try {
    expect(await mintWtToken(MINT)).toEqual({ token: "", authRequired: false });
  } finally {
    restore();
  }
});

test("a minted token comes back with no auth verdict", async () => {
  const restore = respondWith(Response.json({ token: "gmw_abc" }));
  try {
    expect(await mintWtToken(MINT)).toEqual({
      token: "gmw_abc",
      authRequired: false,
    });
  } finally {
    restore();
  }
});

test("no mint configured means authentication is off", async () => {
  expect(await mintWtToken(undefined)).toEqual({
    token: "",
    authRequired: false,
  });
});

// Every credentialed request in the client refuses redirects: a hop that
// bounces the mint to a login page would otherwise answer 200 with a body that
// carries no token, and the failure would read as an unreachable server.
test("a credentialed mint refuses redirects", async () => {
  const real = globalThis.fetch;
  let init: RequestInit | undefined;
  globalThis.fetch = (async (_input: RequestInfo | URL, got?: RequestInit) => {
    init = got;
    return Response.json({ token: "gmw_abc" });
  }) as unknown as typeof fetch;
  try {
    await mintWtToken({ ...MINT, credentials: "include" });
    expect(init?.redirect).toBe("error");
    expect(init?.credentials).toBe("include");
  } finally {
    globalThis.fetch = real;
  }
});

test("withWtToken appends to either URL shape and skips a blank token", () => {
  expect(withWtToken("https://m/wt/ping", "a b")).toBe(
    "https://m/wt/ping?token=a%20b",
  );
  expect(withWtToken("https://m/wt/download?bytes=1", "t")).toBe(
    "https://m/wt/download?bytes=1&token=t",
  );
  expect(withWtToken("https://m/wt/ping", "")).toBe("https://m/wt/ping");
});
