import { describe, expect, test } from "bun:test";
import {
  authenticationRequired,
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

describe("authenticationRequired", () => {
  test("only a 403 carrying the marker counts", () => {
    const response = (status: number, headers: Record<string, string> = {}) =>
      new Response("", { status, headers });

    expect(
      authenticationRequired(
        response(403, { "Graphite-Meter-Auth": "required" }),
      ),
    ).toBe(true);

    // A proxy or WAF answering 403 must not be read as an expired session,
    // or a misconfigured hop becomes a login redirect loop.
    expect(authenticationRequired(response(403))).toBe(false);
    expect(
      authenticationRequired(response(403, { "Graphite-Meter-Auth": "yes" })),
    ).toBe(false);
    expect(
      authenticationRequired(
        response(401, { "Graphite-Meter-Auth": "required" }),
      ),
    ).toBe(false);
    expect(
      authenticationRequired(
        response(200, { "Graphite-Meter-Auth": "required" }),
      ),
    ).toBe(false);
    expect(
      authenticationRequired(
        response(500, { "Graphite-Meter-Auth": "required" }),
      ),
    ).toBe(false);
  });
});

describe("sessionAuthenticationRequired", () => {
  test("recognizes only an exact auth-required response", async () => {
    const answer =
      (status: number, headers: Record<string, string> = {}) =>
      async () =>
        new Response("", { status, headers });

    expect(
      await sessionAuthenticationRequired(
        "https://meter.example",
        undefined,
        answer(403, { "Graphite-Meter-Auth": "required" }),
      ),
    ).toBe(true);

    for (const request of [
      answer(403),
      answer(403, { "Graphite-Meter-Auth": "1" }),
      answer(401, { "Graphite-Meter-Auth": "required" }),
      answer(200, { "Graphite-Meter-Auth": "required" }),
      answer(200),
    ]) {
      expect(
        await sessionAuthenticationRequired(
          "https://meter.example",
          undefined,
          request,
        ),
      ).toBe(false);
    }
  });

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
