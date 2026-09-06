import { expect, test } from "bun:test";
import { stubGlobals } from "../test-helpers.test";
import { TEST_BUILD_TOKENS } from "../runner/test-helpers.test";

test("remote requests omit cookies, reject redirects, and bind the bearer to approved HTTPS destinations", async () => {
  const requests: RequestInit[] = [];
  const restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    location: {
      origin: "https://ui.example",
      href: "https://ui.example/",
      protocol: "https:",
    },
    fetch: async (_url: unknown, init: RequestInit) => {
      requests.push(init);
      return new Response("{}");
    },
  });
  try {
    const { measurementFetch, requestOptions, socketMint } =
      await import("./credentials");
    const context = {
      server: { id: "a", name: "A", url: "https://a.example" },
      kind: "grant" as const,
      token: "private-grant",
      expiresAt: Date.now() + 60000,
    };
    await measurementFetch(context, "https://a.example:8443/probe");
    expect(requests[0].credentials).toBe("omit");
    expect(requests[0].redirect).toBe("error");
    expect(requests[0].headers).toEqual({
      Authorization: "Bearer private-grant",
    });
    expect(() =>
      requestOptions(context, "https://unrelated.example/probe"),
    ).toThrow();
    expect(() => requestOptions(context, "http://a.example/probe")).toThrow(
      "HTTPS",
    );
    const mint = socketMint(context, "https://a.example", "/ping", "ws")!;
    expect(mint.url).not.toContain("private-grant");
    expect(new URL(mint.url).searchParams.get("target")).toBe(
      "https://a.example/ping",
    );
    expect(() =>
      requestOptions(
        { ...context, expiresAt: Date.now() - 1 },
        "https://a.example/probe",
      ),
    ).toThrow("Sign in");
  } finally {
    restore();
  }
});
