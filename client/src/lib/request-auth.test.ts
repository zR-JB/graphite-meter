import { describe, expect, test } from "bun:test";
import { redirectForCredentials } from "./request-auth";

describe("redirectForCredentials", () => {
  test("rejects redirects only when credentials may cross origins", () => {
    expect(redirectForCredentials("include")).toBe("error");
    expect(redirectForCredentials("same-origin")).toBeUndefined();
    expect(redirectForCredentials(undefined)).toBeUndefined();
  });
});
