import "./client-performance";
import {
  fleet,
  fixturePassword,
  stopFleetServer,
  test,
  expect,
} from "./multi-server-fixtures";
import {
  Page,
  AxeBuilder,
  openSettings,
  startTest,
  waitForCompletion,
  expectNoHorizontalOverflow,
} from "../browser/webview";
import { isHistoryRecord } from "../src/lib/history/types";

import { configure, savedResult, ready } from "./multi-server-actions";

test("four real servers share one run and retain separate receiver windows and latency after reload", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await configure(
    page,
    fleet.slice(0, 4).map((server) => server.id),
  );
  await ready(page);
  const selectionSettings = await openSettings(page);
  await expect(
    selectionSettings
      .getByRole("group", { name: "Servers to test", exact: true })
      .getByRole("button")
      .first(),
  ).toBeEnabled();
  await selectionSettings
    .getByRole("button", { name: "Close Settings" })
    .click();
  const startedAt = Date.now();
  await startTest(page);
  await openSettings(page);
  await expect(
    selectionSettings
      .getByRole("group", { name: "Servers to test", exact: true })
      .getByRole("button")
      .first(),
  ).toBeDisabled();
  await selectionSettings
    .getByRole("button", { name: "Close Settings" })
    .click();
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, startedAt);
  expect(isHistoryRecord(saved)).toBe(true);
  expect(saved.schemaVersion).toBe(4);
  expect(saved.multiServer?.participants).toHaveLength(4);
  expect(saved.multiServer?.failures).toEqual([]);
  for (const stage of ["download", "upload", "bidirectional"] as const) {
    const interval = saved.multiServer!.intervals.find(
      (interval) => interval.stage === stage,
    )!;
    expect(interval.complete).toBe(true);
    expect(interval.participants).toHaveLength(4);
    for (const dir of stage === "bidirectional"
      ? (["down", "up"] as const)
      : ([stage === "download" ? "down" : "up"] as const))
      expect(interval.headline?.[dir]).toHaveLength(4);
  }
  for (const server of saved.multiServer!.servers) {
    expect(server.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
    expect(server.totalBytes.down).toBeGreaterThan(0);
    expect(server.totalBytes.up).toBeGreaterThan(0);
  }
  const resultPills = page.getByRole("radiogroup", {
    name: "Result measurements",
  });
  await resultPills
    .getByRole("radio", { name: /All servers, Aggregate throughput/ })
    .focus();
  await page.keyboard.press("ArrowRight");
  await expect(resultPills.getByRole("radio", { name: "Home" })).toBeFocused();
  await expect(
    resultPills.getByRole("radio", { name: "Home" }),
  ).toHaveAttribute("aria-checked", "true");
  await expect(page.getByRole("tooltip")).toContainText("Loopback fixture");
  await page.keyboard.press("End");
  await expect(
    resultPills.getByRole("radio", { name: "Helsinki" }),
  ).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("Home");
  await page.keyboard.press("Escape");
  await expect(
    page.getByRole("radio", { name: /All servers, Aggregate throughput/ }),
  ).toHaveAttribute("aria-checked", "true");
  const audit = await new AxeBuilder({ page })
    .include(".results-slot")
    .analyze();
  expect(audit.violations).toEqual([]);
  await page.artifact("multi-server-desktop-result");
  const settings = await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").click();
  await expect(page.locator(".result-server-context")).toBeVisible();
  await expect(
    page.locator('.result-server-context [role="radio"]'),
  ).toHaveCount(5);
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await page.locator(".result-server-context").scrollIntoViewIfNeeded();
  await page.artifact("multi-server-history-desktop");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.locator(".result-server-context").scrollIntoViewIfNeeded();
  await expectNoHorizontalOverflow(page.locator(".result-server-context"));
  await page.artifact("multi-server-history-phone");
  await page.reload();
  await expect(page.locator(".result-server-context")).toBeVisible();
  expect((await savedResult(page)).multiServer?.intervals).toEqual(
    saved.multiServer?.intervals,
  );
});

test("a single-server result keeps the ordinary live and history views in a fleet", async ({
  page,
}) => {
  await configure(page, ["self"]);
  await ready(page);
  await expect(page.locator(".server-indicator")).toHaveCount(0);
  const startedAt = Date.now();
  await startTest(page);
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, startedAt);
  expect(saved.multiServer?.selection).toHaveLength(1);
  await expect(page.locator(".result-server-context")).toHaveCount(0);
  await expect(page.locator(".server-indicator")).toHaveCount(0);
  const settings = await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").click();
  await expect(page.locator(".result-detail")).toBeVisible();
  await expect(page.locator(".result-server-context")).toHaveCount(0);
  await expect(page.locator(".server-focus")).toHaveCount(0);
  await page.artifact("single-server-fleet-history");
});

test("an origin-only catalogue discovers peer identity and paths without repeated configuration", async ({
  page,
}) => {
  await page.goto(fleet[3].url);
  const settings = await openSettings(page);
  const band = settings.getByRole("group", {
    name: "Servers to test",
    exact: true,
  });
  const peer = band.getByRole("button", { name: "Frankfurt" });
  await expect(peer).toBeVisible({ timeout: 15000 });
  await peer.focus();
  await expect(page.getByRole("tooltip")).toContainText("Loopback fixture");
  await page.artifact("origin-only-peer-discovery");
});

test("a real peer dropout keeps healthy transfers running and persists its failure after reload", async ({
  page,
}) => {
  await configure(
    page,
    fleet.slice(0, 3).map((server) => server.id),
    1500,
    {
      // Leave time for bounded lane recovery, terminal removal, and a fresh survivor window.
      duration: {
        warmupMs: 250,
        latencyMs: 1000,
        downloadMs: 14000,
        uploadMs: 1500,
        bidirectionalMs: 1500,
      },
    },
  );
  await ready(page);
  const startedAt = Date.now();
  await startTest(page);
  await expect(page.locator('[role="status"].label')).toContainText(
    "Downloading",
    {
      timeout: 10000,
    },
  );
  await Bun.sleep(700);
  await stopFleetServer("server-2");
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, startedAt);
  expect(isHistoryRecord(saved)).toBe(true);
  expect(saved.outcome).toBe("partial");
  expect(saved.multiServer?.participants).toEqual(["self", "server-1"]);
  expect(
    saved.multiServer?.failures.find(
      (failure) => failure.scope === "throughput",
    ),
  ).toMatchObject({
    serverId: "server-2",
    scope: "throughput",
    stage: "download",
  });
  const subsequent = saved.multiServer!.intervals.filter(
    (interval) =>
      interval.reason === "dropout" || interval.stage !== "download",
  );
  expect(subsequent.length).toBeGreaterThan(0);
  expect(
    subsequent.every((interval) => !interval.participants.includes("server-2")),
  ).toBe(true);
  expect(saved.stages.upload.result?.reportedBytesPerSec).toBeGreaterThan(0);
  await expect(page.locator(".result-server-context")).toContainText(
    "2 of 3 servers",
  );
  const settings = await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").first().click();
  await page.reload();
  await expect(page.locator(".result-server-context")).toContainText(
    "2 of 3 servers",
  );
  await expect(page.locator(".result-server-context")).toContainText(
    "Amsterdam",
  );
  expect((await savedResult(page)).multiServer?.failures).toEqual(
    saved.multiServer?.failures,
  );
  await page.artifact("multi-server-partial-history");
});

for (const transport of ["websocket", "webtransport"] as const)
  test(`protected peer ${transport} approval works without third-party cookies and with the popup fallback`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.addInitScript(() => {
      window.open = () => null;
    });
    await configure(page, ["self", fleet[4].id], 1800, {
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
      expect(saved.multiServer?.participants).toEqual(["self", fleet[4].id]);
      expect(saved.multiServer?.failures).toEqual([]);
      expect(saved.multiServer?.servers[1].totalBytes.up).toBeGreaterThan(0);
      expect(JSON.stringify(saved)).not.toContain("Bearer");
    } finally {
      approval.close();
    }
  });

for (const theme of ["dark", "light"] as const)
  test(`phone server band supports keyboard, ${theme} theme and reduced motion`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.emulateMedia({ reducedMotion: "reduce" });
    await configure(page, ["self"]);
    await ready(page);
    await page.evaluate(
      (value) => document.documentElement.setAttribute("data-theme", value),
      theme,
    );
    const settings = await openSettings(page);
    const band = settings.getByRole("group", {
      name: "Servers to test",
      exact: true,
    });
    await expect(band.getByRole("button", { name: "Home" })).toBeDisabled();
    const peer = band.getByRole("button", { name: "Frankfurt" });
    await peer.focus();
    await page.keyboard.press("Space");
    await expect(peer).toHaveAttribute("aria-pressed", "true");
    await page.keyboard.press("Escape");
    await openSettings(page);
    await expectNoHorizontalOverflow(settings);
    const scan = await new AxeBuilder({ page })
      .include('[aria-label="Settings"]')
      .analyze();
    expect(scan.violations).toEqual([]);
    await page.artifact(`multi-server-phone-${theme}`);
  });

test("primary latency selection is fixed for the run and saved alongside every throughput participant", async ({
  page,
}) => {
  await configure(page, ["self", "server-1"]);
  await ready(page);
  // Change the primary while the preceding policy's discovery is still in flight.
  await page.evaluate(() => {
    const state = globalThis as typeof globalThis & {
      delayedPreflights: number;
    };
    state.delayedPreflights = 0;
    const browser = window as {
      fetch: (
        input: RequestInfo | URL,
        init?: RequestInit,
      ) => Promise<Response>;
    };
    const original = browser.fetch;
    browser.fetch = async (...args) => {
      if (
        String(args[0]).includes("/preflight?") &&
        state.delayedPreflights < 2
      ) {
        state.delayedPreflights++;
        await new Promise((resolve) => setTimeout(resolve, 600));
      }
      return original(...args);
    };
  });
  const settings = await openSettings(page);
  await settings
    .getByRole("radiogroup", { name: "Latency measurement servers" })
    .getByRole("radio", { name: "Home" })
    .click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (globalThis as typeof globalThis & { delayedPreflights: number })
            .delayedPreflights,
      ),
    )
    .toBe(2);
  await settings
    .getByRole("radiogroup", { name: "Latency measurement servers" })
    .getByRole("radio", { name: "Frankfurt" })
    .click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          JSON.parse(localStorage.getItem("graphite-meter:v1")!)
            .latencySelection.serverId,
      ),
    )
    .toBe("server-1");
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await ready(page);
  const started = Date.now();
  await startTest(page);
  await openSettings(page);
  await expect(
    settings
      .getByRole("radiogroup", { name: "Latency measurement servers" })
      .getByRole("radio", { name: /All servers/ }),
  ).toBeDisabled();
  await expect(
    settings
      .getByRole("radiogroup", { name: "Latency measurement servers" })
      .getByRole("radio", { name: "Home" }),
  ).toBeDisabled();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, started);
  expect(isHistoryRecord(saved)).toBe(true);
  expect(saved.multiServer?.failures).toEqual([]);
  expect(saved.multiServer?.participants).toEqual(["self", "server-1"]);
  const home = saved.multiServer!.servers.find(
    (server) => server.server.id === "self",
  )!;
  const primary = saved.multiServer!.servers.find(
    (server) => server.server.id === "server-1",
  )!;
  expect(home.latencyTarget).toBeNull();
  expect(home.latency).toBeNull();
  expect(
    Object.values(home.latencyByStage).every((value) => value === null),
  ).toBe(true);
  expect(home.totalBytes.down).toBeGreaterThan(0);
  expect(home.totalBytes.up).toBeGreaterThan(0);
  expect(primary.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
  expect(primary.latencyByStage.download?.probeCount).toBeGreaterThan(0);
  expect(saved.multiServer?.latencyFocus).toBe("server-1");
  await expect(page.locator(".latency-focus .server-tag")).toHaveAttribute(
    "aria-label",
    /Frankfurt/,
  );
  await expect(page.locator(".latency-focus [role=radio]")).toHaveCount(0);
  await page
    .getByRole("radiogroup", { name: "Result measurements" })
    .getByRole("radio", { name: "Home" })
    .click();
  await expect(page.locator(".result-cards")).toContainText("Not measured");
  await expect(page.locator(".latency-focus .server-tag")).toHaveAttribute(
    "aria-label",
    /Frankfurt/,
  );
  await page.artifact("primary-latency-result");
  await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").first().click();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await expect(page.locator(".saved-server-context")).toBeVisible();
  await page
    .locator(".saved-server-context")
    .getByRole("radiogroup", { name: "Result measurements" })
    .getByRole("radio", { name: "Frankfurt" })
    .click();
  await expect(
    page
      .locator(".saved-server-context")
      .getByRole("radio", { name: "Frankfurt" }),
  ).toHaveAttribute("aria-checked", "true");
  await page.artifact("primary-latency-history");
});

for (const theme of ["light", "dark"] as const)
  test(`${theme} compact server settings fit touch and keyboard use`, async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.setViewportSize({ width: 390, height: 844 });
    await configure(page, ["self", "server-1"]);
    await ready(page);
    await page.evaluate(
      (value) => document.documentElement.setAttribute("data-theme", value),
      theme,
    );
    const settings = await openSettings(page);
    await settings
      .getByRole("radiogroup", { name: "Latency measurement servers" })
      .getByRole("radio", { name: "Home" })
      .click();
    const primary = settings.getByRole("radiogroup", {
      name: "Latency measurement servers",
    });
    await primary.getByRole("radio", { name: "Home" }).focus();
    await page.keyboard.press("ArrowRight");
    await expect(
      primary.getByRole("radio", { name: "Frankfurt" }),
    ).toBeFocused();
    await expect(
      primary.getByRole("radio", { name: "Frankfurt" }),
    ).toHaveAttribute("aria-checked", "true");
    await page.keyboard.press("Escape");
    await openSettings(page);
    await expectNoHorizontalOverflow(settings);
    const audit = await new AxeBuilder({ page })
      .include('[aria-label="Settings"]')
      .analyze();
    expect(audit.violations).toEqual([]);
    await page.artifact(`compact-server-settings-phone-${theme}`);
  });
