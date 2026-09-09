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

test("browser approval binds a fresh verifier and rejects invalid or canceled exchanges", async () => {
  const server = { id: "peer", name: "Peer", url: "https://peer.example" };
  const requests: { url: string; init: RequestInit }[] = [];
  let payload: unknown = { token: "a".repeat(43), remainingMs: 60000 };
  let status = 200;
  let cancel: (() => void) | undefined;
  const restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    location: new URL("https://ui.example/"),
    fetch: async (url: unknown, init: RequestInit) => {
      requests.push({ url: String(url), init });
      cancel?.();
      return Response.json(payload, { status });
    },
  });
  try {
    const { browserApproval, BrowserApprovalLimitError } =
      await import("./credentials");
    const first = await browserApproval(server);
    const second = await browserApproval(server);
    expect(first.url).not.toBe(second.url);
    expect(first.code).toMatch(/^[A-Z2-7]{8}$/);
    expect(first.code).not.toBe(second.code);
    expect(new URL(first.url).searchParams.get("client_origin")).toBe(
      "https://ui.example",
    );
    const grant = await first.poll(new AbortController().signal);
    expect(grant).toMatchObject({
      server,
      kind: "grant",
      token: "a".repeat(43),
    });
    const request = requests[0];
    expect(request.url).toBe("https://peer.example/auth/browser/token");
    expect(request.init.credentials).toBe("omit");
    expect(request.init.redirect).toBe("error");
    const { verifier } = JSON.parse(String(request.init.body));
    const challenge = Buffer.from(
      await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)),
    ).toString("base64url");
    expect(new URL(first.url).searchParams.get("challenge")).toBe(challenge);
    expect(first.url).not.toContain(verifier);
    for (const invalid of [
      { token: "short", remainingMs: 60000 },
      { token: "a".repeat(43), remainingMs: 0 },
      { token: "a".repeat(43), remainingMs: 8 * 3600000 + 1 },
    ]) {
      payload = invalid;
      await expect(second.poll(new AbortController().signal)).rejects.toThrow(
        "Invalid measurement grant",
      );
    }
    status = 429;
    await expect(
      second.poll(new AbortController().signal),
    ).rejects.toBeInstanceOf(BrowserApprovalLimitError);
    status = 200;
    payload = { token: "a".repeat(43), remainingMs: 60000 };
    const abort = new AbortController();
    cancel = () => abort.abort(new Error("Canceled sign-in"));
    await expect(second.poll(abort.signal)).rejects.toThrow("Canceled sign-in");
    const before = requests.length;
    await expect(second.poll(abort.signal)).rejects.toThrow("Canceled sign-in");
    expect(requests).toHaveLength(before);
    await expect(
      browserApproval({ ...server, url: "http://peer.example" }),
    ).rejects.toThrow("HTTPS");
  } finally {
    restore();
  }
});

test("approval popup reserves an isolated window synchronously and survives browser restrictions", async () => {
  const { openBrowserApprovalPopup } = await import("./credentials");
  for (const mode of [
    "open",
    "blocked",
    "throws",
    "navigation-denied",
    "close-denied",
  ] as const) {
    let opens = 0;
    let navigations = 0;
    let closes = 0;
    const popup = {
      opener: {} as unknown,
      location: {
        replace(url: string) {
          expect(popup.opener).toBeNull();
          expect(url).toBe(
            "https://peer.example/auth/browser?challenge=public",
          );
          if (mode === "navigation-denied") throw new Error("Blocked");
          navigations++;
        },
      },
      close() {
        closes++;
        if (mode === "close-denied") throw new Error("Isolated");
      },
    };
    const restore = stubGlobals({
      window: {
        open(url: string, target: string, features: string) {
          opens++;
          expect(url).toBe("about:blank");
          expect(target).toBe("_blank");
          expect(features).toContain("popup");
          if (mode === "throws") throw new Error("Blocked");
          return mode === "blocked" ? null : popup;
        },
      },
    });
    try {
      const task = new AbortController();
      const handle = openBrowserApprovalPopup(task.signal);
      expect(opens).toBe(1);
      handle.navigate("https://peer.example/auth/browser?challenge=public");
      expect(navigations).toBe(
        mode === "open" || mode === "close-denied" ? 1 : 0,
      );
      task.abort();
      handle.navigate("https://peer.example/auth/browser?challenge=public");
      handle.close();
      expect(closes).toBe(
        mode === "open" ||
          mode === "navigation-denied" ||
          mode === "close-denied"
          ? 1
          : 0,
      );
      openBrowserApprovalPopup(task.signal).close();
      expect(opens).toBe(1);
    } finally {
      restore();
    }
  }
});

test("session fetches enforce the same selected-server boundary as grants", async () => {
  const requests: string[] = [];
  const restore = stubGlobals({
    location: new URL("https://home.example/"),
    fetch: async (input: unknown) => {
      requests.push(String(input));
      return new Response("{}");
    },
  });
  try {
    const { measurementFetch } = await import("./credentials");
    const session = {
      server: { id: "self", name: "Home", url: "https://home.example" },
      kind: "session" as const,
    };
    await expect(
      measurementFetch(session, "https://unrelated.example/probe"),
    ).rejects.toThrow("outside the selected server");
    expect(requests).toHaveLength(0);
    await measurementFetch(session, "https://home.example:8443/probe");
    expect(requests).toEqual(["https://home.example:8443/probe"]);
  } finally {
    restore();
  }
});
