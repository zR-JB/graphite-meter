import { test, expect } from "bun:test";
import { mintWtToken, spendWtToken, withWtToken } from "./wtToken";
import { ESTABLISH_BUDGET_MS, LANE_RESTART_BACKOFF_MS } from "../real/budgets";
import { stubFetch } from "./test-helpers.test";

const MINT = { url: "https://meter.test/wt/session" };

function respondWith(response: Response): () => void {
  return stubFetch((async () => response) as unknown as typeof fetch);
}

function captureFetch(): {
  init: () => RequestInit | undefined;
  restore: () => void;
} {
  let init: RequestInit | undefined;
  const restore = stubFetch((async (
    _input: RequestInfo | URL,
    got?: RequestInit,
  ) => {
    init = got;
    return Response.json({ token: "gmw_abc", expires: 0 });
  }) as unknown as typeof fetch);
  return { init: () => init, restore };
}

function expectMint(
  response: Response,
  expected: { token: string; authRequired: boolean },
): Promise<void> {
  const restore = respondWith(response);
  return mintWtToken(MINT)
    .then((result) => expect(result).toEqual(expected))
    .finally(restore);
}

test("a mint refusal carrying the marker reports the session as gone", async () => {
  await expectMint(
    new Response("no", {
      status: 403,
      headers: { "Graphite-Meter-Auth": "required" },
    }),
    { token: "", authRequired: true },
  );
});

test("a bare refusal is a retry, not a login", async () => {
  await expectMint(new Response("no", { status: 403 }), {
    token: "",
    authRequired: false,
  });
});

test("a minted token comes back with no auth verdict", async () => {
  await expectMint(Response.json({ token: "gmw_abc", expires: 0 }), {
    token: "gmw_abc",
    authRequired: false,
  });
});

test("no mint configured means authentication is off", async () => {
  expect(await mintWtToken(undefined)).toEqual({
    token: "",
    authRequired: false,
  });
});

test("a credentialed mint is an uncached POST with caller headers and no redirects", async () => {
  const capture = captureFetch();
  try {
    await mintWtToken({
      ...MINT,
      credentials: "include",
      headers: { "X-CSRF-Token": "csrf-token" },
    });
    expect(capture.init()).toMatchObject({
      method: "POST",
      cache: "no-store",
      redirect: "error",
      credentials: "include",
      headers: { "X-CSRF-Token": "csrf-token" },
    });
  } finally {
    capture.restore();
  }
});

function respondOnAbort(): {
  seen: () => AbortSignal | undefined;
  restore: () => void;
} {
  let seen: AbortSignal | undefined;
  const restore = stubFetch(
    ((_input: RequestInfo | URL, got?: RequestInit) =>
      new Promise<Response>((_, reject) => {
        seen = got?.signal ?? undefined;
        seen?.addEventListener("abort", () => reject(new Error("aborted")));
      })) as unknown as typeof fetch,
  );
  return { seen: () => seen, restore };
}

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

function countingMint(): {
  calls: () => number;
  restore: () => void;
} {
  let calls = 0;
  const restore = stubFetch((async () => {
    calls++;
    return Response.json({ token: "gmw_live", expires: Date.now() + 30_000 });
  }) as unknown as typeof fetch);
  return { calls: () => calls, restore };
}

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

test("worker mint cancels oversized streamed control bodies", async () => {
  let canceled = false;
  let reads = 0;
  const restore = respondWith(
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
  );
  try {
    expect(await mintWtToken({ url: "https://meter.test/oversized" })).toEqual({
      token: "",
      authRequired: false,
    });
    expect(canceled).toBe(true);
    expect(reads).toBeLessThanOrEqual(6);
  } finally {
    restore();
  }
});

test("malformed mint tokens and overflowing JSON expiry are refused without inventing auth failure", async () => {
  for (const body of [
    "null",
    '{"token":"' + "a".repeat(8193) + '"}',
    '{"token":"gmw_bad","expires":1e999}',
    '{"token":"gmw_bad","expires":null}',
  ]) {
    const restore = respondWith(new Response(body));
    try {
      expect(await mintWtToken({ url: "https://meter.test/invalid" })).toEqual({
        token: "",
        authRequired: false,
      });
    } finally {
      restore();
    }
  }
});

test("expiry-free mint responses are rejected", async () => {
  let calls = 0;
  const restore = stubFetch((async () => {
    calls++;
    return Response.json({ token: "gmw_legacy" });
  }) as unknown as typeof fetch);
  try {
    const mint = { url: "https://meter.test/legacy" };
    expect((await mintWtToken(mint)).token).toBe("");
    expect((await mintWtToken(mint)).token).toBe("");
    expect(calls).toBe(2);
  } finally {
    restore();
  }
});
