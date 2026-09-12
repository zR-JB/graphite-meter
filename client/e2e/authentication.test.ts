import "./auth-popup";
import { isHistoryRecord } from "../src/lib/history/types";
import { fleet, fixturePassword, test, expect } from "./multi-server-fixtures";
import {
  Page,
  openSettings,
  startTest,
  waitForCompletion,
} from "../browser/webview";
import { configure, savedResult, ready } from "./multi-server-actions";

test("an HTTP interface explains an opaque protected HTTPS refusal while public HTTPS stays usable", async ({
  page,
}) => {
  const protectedServer = fleet[4];
  const refusal = await fetch(`${protectedServer.url}/preflight`, {
    headers: { Origin: fleet[0].http },
    tls: { rejectUnauthorized: false },
  });
  expect(refusal.status).toBe(403);
  expect(refusal.headers.get("Graphite-Meter-Auth")).toBe("required");
  expect(refusal.headers.get("Access-Control-Allow-Origin")).toBeNull();
  await refusal.body?.cancel();
  await page.addInitScript(
    ({ protectedOrigin, unmarkedOrigin }) => {
      const original = window.fetch.bind(window);
      const evidence = { protectedFailure: "" };
      Object.assign(window, { httpAuthEvidence: evidence });
      window.fetch = (async (input, init) => {
        const url = new URL(
          input instanceof Request ? input.url : String(input),
          location.href,
        );
        if (url.pathname === "/preflight" && url.origin === unmarkedOrigin)
          return new Response(null, { status: 403 });
        try {
          return await original(input, init);
        } catch (error) {
          if (url.pathname === "/preflight" && url.origin === protectedOrigin)
            evidence.protectedFailure =
              error instanceof Error ? error.name : String(error);
          throw error;
        }
      }) as typeof window.fetch;
    },
    { protectedOrigin: protectedServer.url, unmarkedOrigin: fleet[2].url },
  );
  await configure(
    page,
    ["self", fleet[1].id, fleet[2].id, protectedServer.id],
    1500,
    {},
    { mode: "primary", serverId: "self" },
    fleet[0].http,
  );
  const settings = await openSettings(page);
  const protectedFeedback = settings.locator(".server-feedback", {
    hasText: "Private",
  });
  await expect(protectedFeedback).toContainText(
    "If it requires sign-in, open this interface over HTTPS",
  );
  await expect(
    protectedFeedback.getByRole("button", { name: "Retry Private" }),
  ).toBeVisible();
  await expect(
    settings.getByRole("button", { name: "Sign in to Private" }),
  ).toHaveCount(0);
  await expect(
    settings.locator(".server-choices label", { hasText: "Private" }),
  ).toContainText("Unavailable");
  await expect(
    settings
      .locator(".server-choices label", { hasText: "Frankfurt" })
      .locator('.server-status[data-state="ready"]'),
  ).toBeVisible({ timeout: 15000 });
  const unmarkedFeedback = settings.locator(".server-feedback", {
    hasText: "Amsterdam",
  });
  await expect(unmarkedFeedback).toContainText("Connection check failed");
  await expect(
    unmarkedFeedback.getByRole("button", { name: "Retry Amsterdam" }),
  ).toBeVisible();
  await expect(
    settings.getByRole("button", { name: "Sign in to Amsterdam" }),
  ).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (
          window as unknown as {
            httpAuthEvidence: { protectedFailure: string };
          }
        ).httpAuthEvidence.protectedFailure,
    ),
  ).toBe("TypeError");
});

test("a full remote login offers explicit renewal and discards the old approval link", async ({
  page,
}) => {
  await page.addInitScript((peer) => {
    window.open = () => null;
    const original = window.fetch.bind(window);
    window.fetch = ((input, init) => {
      const url = new URL(String(input), location.href);
      return url.origin === peer && url.pathname === "/auth/browser/token"
        ? Promise.resolve(new Response(null, { status: 429 }))
        : original(input, init);
    }) as typeof window.fetch;
  }, fleet[4].url);
  await configure(page, ["self", fleet[4].id]);
  await openSettings(page);
  const row = page.locator(".server-feedback", { hasText: "Private" });
  await row.getByRole("button", { name: "Sign in to Private" }).click();
  const renewal = row.getByRole("link", { name: "Renew login at Private" });
  await expect(renewal).toBeVisible();
  await expect(renewal).toHaveAttribute("href", `${fleet[4].url}/login`);
  await expect(renewal).toHaveAttribute("rel", "noopener noreferrer");
  await expect(row).toContainText(
    "Renewing ends the other client connections authorized by that login",
  );
  await expect(
    row.getByRole("link", { name: "Open sign-in page" }),
  ).toHaveCount(0);
  await row.getByRole("button", { name: "Cancel sign-in" }).click();
  await expect(renewal).toHaveCount(0);
  await expect(
    row.getByRole("button", { name: "Sign in to Private" }),
  ).toBeEnabled();
});

for (const transport of ["websocket", "webtransport"] as const)
  test(`protected peer ${transport} approval works without third-party cookies through the isolated sign-in link`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 900 });
    const ids =
      transport === "websocket"
        ? ["self", "server-1", "server-2", fleet[4].id]
        : ["self", fleet[4].id];
    await page.addInitScript(
      (unavailable) => {
        window.open = () => null;
        const original = window.fetch.bind(window);
        window.fetch = ((input, init) => {
          const url = new URL(
            input instanceof Request ? input.url : String(input),
            location.href,
          );
          if (url.pathname === "/probe" && unavailable.includes(url.origin))
            return Promise.reject(new TypeError("Fixture path unavailable"));
          return original(input, init);
        }) as typeof window.fetch;
      },
      transport === "websocket" ? [fleet[1].h3, fleet[2].h3, fleet[2].h2] : [],
    );
    await configure(page, ids, 1800, {
      transports: {
        throughputTarget:
          transport === "webtransport" ? "transport:webtransport" : "auto",
        latencyTarget: `transport:${transport}`,
      },
    });
    await page.raw.cdp("Network.setCookieControls", {
      enableThirdPartyCookieRestriction: true,
      disableThirdPartyCookieMetadata: true,
      disableThirdPartyCookieHeuristics: true,
    });
    await openSettings(page);
    const row = page.locator(".server-feedback", { hasText: "Private" });
    await expect(
      row.getByRole("button", { name: "Sign in to Private" }),
    ).toBeVisible({ timeout: 15000 });
    await row.getByRole("button", { name: "Sign in to Private" }).click();
    const link = row.getByRole("link", { name: "Open sign-in page" });
    await expect(link).toBeVisible();
    const cancelledURL = await link.getAttribute("href");
    await row.getByRole("button", { name: "Cancel sign-in" }).click();
    await expect(link).toHaveCount(0);
    await row.getByRole("button", { name: "Sign in to Private" }).click();
    await expect(link).toBeVisible();
    expect(await link.getAttribute("href")).not.toBe(cancelledURL);
    const comparisonCode = await row
      .locator(".approval-code strong")
      .textContent();
    expect(comparisonCode).toMatch(/^[A-Z2-7]{8}$/);
    await page.evaluate((origin) => {
      const fetch = window.fetch.bind(window);
      let failOnce = true;
      window.fetch = ((input, init) => {
        const url = new URL(
          input instanceof Request ? input.url : String(input),
          location.href,
        );
        if (
          failOnce &&
          url.origin === origin &&
          url.pathname === "/preflight" &&
          new Headers(init?.headers).has("Authorization")
        ) {
          failOnce = false;
          return Promise.resolve(
            Response.json({ error: "Temporary outage" }, { status: 503 }),
          );
        }
        return fetch(input, init);
      }) as typeof window.fetch;
    }, fleet[4].url);
    const approval = new Page();
    try {
      await approval.goto((await link.getAttribute("href"))!);
      await approval.getByLabel("Operator password").fill(fixturePassword);
      await approval
        .getByRole("button", { name: "Sign in with operator password" })
        .click();
      await expect(
        approval.getByRole("heading", { name: "Approve browser client" }),
      ).toBeVisible();
      await expect(approval.locator("main")).toContainText(fleet[0].url);
      await expect(approval.locator("main")).toContainText(comparisonCode!);
      await approval
        .getByRole("button", { name: "Approve this client" })
        .click();
      // A connection error after approval must keep the accepted grant and offer
      // a path retry, instead of incorrectly asking the user to sign in again.
      await expect(
        row.getByRole("button", { name: "Retry Private" }),
      ).toBeVisible();
      await expect(
        row.getByRole("button", { name: "Sign in to Private" }),
      ).toHaveCount(0);
      await row.getByRole("button", { name: "Retry Private" }).click();
      await expect(row).toHaveCount(0);
      await ready(page);
      // Every WebView shares Chromium's cookie jar. Remove cookies locally without
      // logging out the parent session; the requesting page must rely on its grant.
      await page.raw.cdp("Network.clearBrowserCookies");
      const cookies = await page.raw.cdp<{ cookies: unknown[] }>(
        "Network.getCookies",
        { urls: [fleet[4].url] },
      );
      expect(cookies.cookies).toEqual([]);
      const startedAt = Date.now();
      await startTest(page);
      await waitForCompletion(page, 30000);
      const saved = await savedResult(page, startedAt);
      expect(isHistoryRecord(saved)).toBe(true);
      if (saved.multiServer?.failures.length)
        console.info(
          "Protected mixed-protocol failures",
          saved.multiServer.failures,
        );
      expect(saved.multiServer?.participants).toEqual(ids);
      expect(saved.multiServer?.failures).toEqual([]);
      for (const server of saved.multiServer!.servers) {
        expect(server.totalBytes.down).toBeGreaterThan(0);
        expect(server.totalBytes.up).toBeGreaterThan(0);
      }
      if (transport === "websocket")
        expect(
          saved.multiServer!.servers.map((server) => server.throughput?.origin),
        ).toEqual([fleet[0].h3, fleet[1].h2, fleet[2].url, fleet[4].h3]);
      expect(JSON.stringify(saved)).not.toContain("Bearer");
    } finally {
      approval.close();
    }
  });
