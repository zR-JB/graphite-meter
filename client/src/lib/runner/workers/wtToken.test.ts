import { test, expect } from "bun:test";
import { mintWtToken, spendWtToken, withWtToken } from "./wtToken";
import { ESTABLISH_BUDGET_MS, LANE_RESTART_BACKOFF_MS } from "../real/budgets";

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

// The mint is a state-changing POST under the boundary's own rules: without the
// double-submit header mutationOriginAllowed refuses every one of them, a GET
// is answered 405, and a cached response would hand back a token an earlier
// dial already spent.
test("a mint is an uncached POST carrying the caller's headers", async () => {
  const real = globalThis.fetch;
  let init: RequestInit | undefined;
  globalThis.fetch = (async (_input: RequestInfo | URL, got?: RequestInit) => {
    init = got;
    return Response.json({ token: "gmw_abc" });
  }) as unknown as typeof fetch;
  try {
    await mintWtToken({ ...MINT, headers: { "X-CSRF-Token": "csrf-token" } });
    expect(init?.method).toBe("POST");
    expect(init?.cache).toBe("no-store");
    expect(init?.headers).toEqual({ "X-CSRF-Token": "csrf-token" });
  } finally {
    globalThis.fetch = real;
  }
});

/** A fetch that answers only when its signal aborts, so a test can prove the
 *  mint carries a bound of its own. */
function respondOnAbort(): {
  seen: () => AbortSignal | undefined;
  restore: () => void;
} {
  const real = globalThis.fetch;
  let seen: AbortSignal | undefined;
  globalThis.fetch = ((_input: RequestInfo | URL, got?: RequestInit) =>
    new Promise<Response>((_, reject) => {
      seen = got?.signal ?? undefined;
      seen?.addEventListener("abort", () => reject(new Error("aborted")));
    })) as unknown as typeof fetch;
  return {
    seen: () => seen,
    restore: () => {
      globalThis.fetch = real;
    },
  };
}

// A mint that never answers holds the dial open past the point its token would
// have expired, so it carries a bound whether or not the caller supplies one.
test("a mint that never answers is abandoned on its own bound", async () => {
  const hang = respondOnAbort();
  try {
    expect(await mintWtToken({ url: "https://meter.test/hangs" })).toEqual({
      token: "",
      authRequired: false,
    });
  } finally {
    hang.restore();
  }
}, 10_000);

// The caller's signal has to cut the mint short without displacing that bound,
// which is what makes the two an "any" rather than a choice.
test("a caller's signal cuts the mint short", async () => {
  const hang = respondOnAbort();
  const caller = new AbortController();
  try {
    const pending = mintWtToken(
      { url: "https://meter.test/hangs-too" },
      caller.signal,
    );
    expect(hang.seen()).toBeInstanceOf(AbortSignal);
    expect(hang.seen()).not.toBe(caller.signal);
    caller.abort();
    expect(await pending).toEqual({ token: "", authRequired: false });
  } finally {
    hang.restore();
  }
});

/** Count mints, answering each with a live token. */
function countingMint(): {
  calls: () => number;
  restore: () => void;
} {
  const real = globalThis.fetch;
  let calls = 0;
  globalThis.fetch = (async () => {
    calls++;
    return Response.json({ token: "gmw_live", expires: Date.now() + 30_000 });
  }) as unknown as typeof fetch;
  return {
    calls: () => calls,
    restore: () => {
      globalThis.fetch = real;
    },
  };
}

// A dial that fails before its CONNECT is accepted leaves the token unspent, so
// the retry carries the same one. Minting per attempt fills the session's cap
// of eight within a couple of seconds, and every stage and tab of that login is
// refused for the rest of the token lifetime.
test("a re-dial reuses the token the failed dial never spent", async () => {
  const url = "https://meter.test/reused";
  const mint = countingMint();
  try {
    const first = await mintWtToken({ url });
    expect(first.token).toBe("gmw_live");
    for (let i = 0; i < 8; i++) {
      expect((await mintWtToken({ url })).token).toBe("gmw_live");
    }
    expect(mint.calls()).toBe(1);
  } finally {
    mint.restore();
  }
});

// The window is what bounds a dial the server accepted but the client never saw
// resolve: the token is gone server-side and reuse would replay a dead one. It
// is sized to expire by the time the retry runs — the establish budget the dial
// burned plus the restart backoff — so widening it reintroduces that replay.
test("the reuse window expires by the retry that follows a failed dial", async () => {
  const url = "https://meter.test/window";
  const mint = countingMint();
  try {
    expect((await mintWtToken({ url })).token).toBe("gmw_live");
    await Bun.sleep(ESTABLISH_BUDGET_MS + LANE_RESTART_BACKOFF_MS + 10);
    expect((await mintWtToken({ url })).token).toBe("gmw_live");
    expect(mint.calls()).toBe(2);
  } finally {
    mint.restore();
  }
}, 10_000);

// A CONNECT the server accepted spends the token, so the next dial must not
// carry it: the server has already deleted it and would refuse the replay.
test("a spent token is never handed out again", async () => {
  const url = "https://meter.test/spent";
  const mint = countingMint();
  try {
    const first = await mintWtToken({ url });
    spendWtToken(first.token);
    expect((await mintWtToken({ url })).token).toBe("gmw_live");
    expect(mint.calls()).toBe(2);
  } finally {
    mint.restore();
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
