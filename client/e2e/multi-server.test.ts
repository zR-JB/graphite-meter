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
  await expect(
    page.getByRole("button", { name: /Servers · 4 selected/ }),
  ).toBeEnabled();
  const startedAt = Date.now();
  await startTest(page);
  await expect(
    page.getByRole("button", { name: /Servers · 4 selected/ }),
  ).toBeDisabled();
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
  await page.artifact("multi-server-desktop-result");
  const settings = await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").click();
  await expect(page.locator(".server-results")).toBeVisible();
  await page.locator(".server-results > summary").click();
  await expect(page.locator(".server-results tbody tr")).toHaveCount(4);
  await page.reload();
  await expect(page.locator(".server-results")).toBeVisible();
  expect((await savedResult(page)).multiServer?.intervals).toEqual(
    saved.multiServer?.intervals,
  );
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
  await expect(page.locator(".server-results > summary")).toContainText(
    "2 of 3 servers",
  );
  const settings = await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").first().click();
  await page.reload();
  await expect(page.locator(".server-results > summary")).toContainText(
    "2 of 3 servers",
  );
  await page.locator(".server-results > summary").click();
  await expect(page.locator(".server-results")).toContainText("Amsterdam");
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
    await page.getByRole("button", { name: /Servers · 2 selected/ }).click();
    const row = page.locator(".server-row", { hasText: "Private" });
    await expect(row.getByRole("button", { name: "Sign in…" })).toBeVisible({
      timeout: 15000,
    });
    await row.getByRole("button", { name: "Sign in…" }).click();
    const link = row.getByRole("link", { name: "Open sign-in page" });
    await expect(link).toBeVisible();
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
      await expect(row.locator(".readiness")).toHaveText("Ready");
      await page.getByRole("button", { name: "Apply", exact: true }).click();
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
  test(`phone server chooser supports keyboard, ${theme} theme and reduced motion`, async ({
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
    const trigger = page.getByRole("button", { name: /Servers · 1 selected/ });
    await trigger.focus();
    await page.keyboard.press("Enter");
    const dialog = page.getByRole("dialog", { name: "Servers", exact: true });
    await expect(dialog).toBeVisible();
    await expectNoHorizontalOverflow(dialog);
    const first = dialog.locator('input[type="checkbox"]').first();
    await first.focus();
    await page.keyboard.press("Space");
    await expect(
      dialog.getByRole("button", { name: "Apply", exact: true }),
    ).toBeDisabled();
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(trigger).toBeFocused();
    await trigger.click();
    await expect(dialog).toBeVisible();
    const scan = await new AxeBuilder({ page })
      .include(".server-dialog")
      .analyze();
    expect(scan.violations).toEqual([]);
    await page.artifact(`multi-server-phone-${theme}`);
  });
