import { describe, expect, test } from "bun:test";
import {
  redirectForCredentials,
  sessionAuthenticationRequired,
} from "./request-auth";

describe("redirectForCredentials", () => {
  test("rejects redirects only when credentials may cross origins", () => {
    expect(redirectForCredentials("include")).toBe("error");
    expect(redirectForCredentials("same-origin")).toBeUndefined();
    expect(redirectForCredentials(undefined)).toBeUndefined();
  });
});

describe("sessionAuthenticationRequired", () => {
  test.each([
    [403, "required", true],
    [403, null, false],
    [403, "yes", false],
    [401, "required", false],
    [200, "required", false],
    [500, "required", false],
    [200, null, false],
  ] as const)(
    "session status %i with marker %s means expiry: %s",
    async (status, marker, expired) => {
      const result = await sessionAuthenticationRequired(
        "https://meter.example",
        undefined,
        async (url, init) => {
          expect(String(url)).toBe("https://meter.example/auth/session");
          expect(init).toMatchObject({
            cache: "no-store",
            credentials: "include",
            redirect: "error",
          });
          return new Response(null, {
            status,
            headers: marker ? { "Graphite-Meter-Auth": marker } : {},
          });
        },
      );
      expect(result).toBe(expired);
    },
  );

  test("network failures and deliberate aborts are not expiry evidence", async () => {
    const failed = async () => {
      throw new TypeError("network failure");
    };
    expect(
      await sessionAuthenticationRequired(
        "https://meter.example",
        undefined,
        failed,
      ),
    ).toBe(false);
    const controller = new AbortController();
    controller.abort();
    let called = false;
    expect(
      await sessionAuthenticationRequired(
        "https://meter.example",
        controller.signal,
        async () => {
          called = true;
          return new Response();
        },
      ),
    ).toBe(false);
    expect(called).toBe(false);
  });
});

test("auth classification reads only headers and cancels the unused body", async () => {
  let canceled = false;
  const response = new Response(
    new ReadableStream({
      cancel() {
        canceled = true;
      },
    }),
    {
      status: 403,
      headers: { "Graphite-Meter-Auth": "required" },
    },
  );
  expect(
    await sessionAuthenticationRequired(
      "https://meter.test",
      undefined,
      async () => response,
    ),
  ).toBe(true);
  expect(canceled).toBe(true);
});

test("canceling a classification aborts its request without reporting expiry", async () => {
  const caller = new AbortController();
  let requestSignal: AbortSignal | null = null;
  const pending = sessionAuthenticationRequired(
    "https://meter.test",
    caller.signal,
    async (_input, init) => {
      requestSignal = init!.signal!;
      return new Promise<Response>((_resolve, reject) =>
        requestSignal!.addEventListener("abort", () =>
          reject(new Error("aborted")),
        ),
      );
    },
  );
  caller.abort();
  expect(await pending).toBe(false);
  expect(requestSignal!.aborted).toBe(true);
});

test("an explicit auth marker survives failure while discarding its body", async () => {
  const response = new Response(
    new ReadableStream({
      cancel() {
        throw new Error("body failed");
      },
    }),
    {
      status: 403,
      headers: { "Graphite-Meter-Auth": "required" },
    },
  );
  expect(
    await sessionAuthenticationRequired(
      "https://meter.test",
      undefined,
      async () => response,
    ),
  ).toBe(true);
});
