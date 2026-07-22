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
  test("recognizes only an exact auth-required response", async () => {
    const request = async () =>
      new Response("", {
        status: 403,
        headers: { "Graphite-Meter-Auth": "required" },
      });
    expect(
      await sessionAuthenticationRequired(
        "https://meter.example",
        undefined,
        request,
      ),
    ).toBe(true);
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
