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
    page.getByRole("button", { name: /Change servers, 4 selected/ }),
  ).toBeEnabled();
  await selectionSettings
    .getByRole("button", { name: "Close Settings" })
    .click();
  const startedAt = Date.now();
  await startTest(page);
  await openSettings(page);
  await expect(
    page.getByRole("button", { name: /Change servers, 4 selected/ }),
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
    .getByRole("radio", { name: "All servers, aggregate throughput" })
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
    page.getByRole("radio", { name: "All servers, aggregate throughput" }),
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
  await settings
    .getByRole("button", { name: "Change servers, 1 selected" })
    .click();
  const dialog = page.getByRole("dialog", {
    name: "Choose servers",
    exact: true,
  });
  const peer = dialog.locator(".server-row").nth(1);
  await expect(peer).toContainText(fleet[1].name, { timeout: 15000 });
  await expect(peer).toContainText("Loopback fixture");
  await expect(peer.locator(".readiness")).toHaveText("Ready", {
    timeout: 15000,
  });
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
    await page
      .getByRole("button", { name: /Change servers, 2 selected/ })
      .click();
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
    await openSettings(page);
    const trigger = page.getByRole("button", {
      name: /Change servers, 1 selected/,
    });
    await trigger.focus();
    await page.keyboard.press("Enter");
    const dialog = page.getByRole("dialog", {
      name: "Choose servers",
      exact: true,
    });
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

test("primary latency selection is fixed for the run and saved alongside every throughput participant", async ({
  page,
}) => {
  await configure(page, ["self", "server-1"]);
  await ready(page);
  const settings = await openSettings(page);
  await settings
    .getByRole("button", { name: "One server", exact: true })
    .click();
  await settings
    .getByRole("radiogroup", { name: "Primary latency server" })
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
    settings.getByRole("button", { name: "Every server", exact: true }),
  ).toBeDisabled();
  await expect(
    settings
      .getByRole("radiogroup", { name: "Primary latency server" })
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
  await expect(page.locator(".latency-focus")).toContainText("Frankfurt");
  await expect(page.locator(".latency-focus [role=radio]")).toHaveCount(0);
  await page
    .getByRole("radiogroup", { name: "Result measurements" })
    .getByRole("radio", { name: "Home" })
    .click();
  await expect(page.locator(".result-cards")).toContainText(
    "Not measured for this server",
  );
  await expect(page.locator(".latency-focus")).toContainText("Frankfurt");
  await page.artifact("primary-latency-result");
  await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").first().click();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await expect(page.locator(".saved-server-context")).toContainText(
    "One latency server",
  );
  await page
    .locator(".saved-server-context")
    .getByRole("radiogroup", { name: "Result measurements" })
    .getByRole("radio", { name: "Frankfurt" })
    .click();
  await expect(page.locator(".saved-server-context")).toContainText(
    "Frankfurt",
  );
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
      .getByRole("button", { name: "One server", exact: true })
      .click();
    const primary = settings.getByRole("radiogroup", {
      name: "Primary latency server",
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
