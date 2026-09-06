import {
  test,
  expect,
  openApp,
  openSettings,
  expectNoHorizontalOverflow,
  AxeBuilder,
} from "./webview";

for (const theme of ["dark", "light"] as const) {
  test(`server selection fits the instrument and settings in ${theme} theme`, async ({
    page,
  }) => {
    await page.addInitScript(() =>
      localStorage.setItem(
        "graphite-meter:v1",
        JSON.stringify({
          config: {
            stages: {
              latency: false,
              upload: false,
              download: true,
              bidirectional: false,
            },
            skipLoadedLatencyWhenStageOff: true,
          },
        }),
      ),
    );
    await page.route("**/servers", (route) =>
      route.fulfill({
        json: {
          defaultSelection: ["self", "peer"],
          servers: [
            { id: "self", url: ".", name: "Local" },
            { id: "peer", url: "http://localhost:45678", name: "Peer" },
          ],
        },
      }),
    );
    await page.route("**/preflight?*", (route) =>
      route.fulfill({
        json: {
          server: { name: "Frankfurt", location: "Germany" },
          engineVersion: "test",
          generation: "selection-test",
          capabilities: {
            uploadCheckpoint: true,
            throughput: [
              {
                baseUrl: ".",
                transport: "fetch-stream",
                protocol: "negotiated",
              },
            ],
            latency: [],
          },
        },
      }),
    );
    await page.route("**/probe?*", (route) =>
      route.fulfill({
        json: {
          clientIp: "127.0.0.1",
          clientIpVersion: 4,
          clientIpSource: "socket",
          protocolNegotiated: "http/1.1",
        },
      }),
    );
    await openApp(page, "real", { width: 1440, height: 900 });
    await page.evaluate(
      (theme) => document.documentElement.setAttribute("data-theme", theme),
      theme,
    );
    await expect(page.locator(".server-indicator")).toHaveText(
      "2 servers selected",
    );
    await expect(page.locator(".run-slot button")).toHaveCount(1);
    await page.artifact(`server-context-desktop-${theme}`);
    const settings = await openSettings(page);
    const trigger = settings.getByRole("button", {
      name: "Change servers, 2 selected",
    });
    await expect(trigger).toBeVisible();
    await trigger.focus();
    await trigger.click();
    const dialog = page.getByRole("dialog", {
      name: "Choose servers",
      exact: true,
    });
    await expect(dialog).toBeVisible();
    await expect(dialog.locator("details")).toHaveCount(0);
    await expect(dialog.locator(".server-row").first()).toContainText(
      "Germany",
    );
    await expectNoHorizontalOverflow(dialog);
    await dialog.evaluate(async (element) => {
      await Promise.all(
        element
          .getAnimations()
          .map((animation: Animation) => animation.finished),
      );
    });
    expect(
      (await new AxeBuilder({ page }).include(".server-dialog").analyze())
        .violations,
    ).toEqual([]);
    await page.artifact(`server-chooser-desktop-${theme}`);
    await page.setViewportSize({ width: 320, height: 740 });
    await expectNoHorizontalOverflow(dialog);
    await page.artifact(`server-chooser-phone-${theme}`);
    await dialog.locator('input[type="checkbox"]').first().focus();
    await page.keyboard.press("Space");
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(trigger).toBeFocused();
    await expect(trigger).toContainText("2 selected");
    await trigger.focus();
    await trigger.click();
    await dialog.locator('input[type="checkbox"]').first().focus();
    await page.keyboard.press("Space");
    await dialog.getByRole("button", { name: "Apply", exact: true }).click();
    await expect(dialog).toBeHidden();
    await expect(page.locator(".server-indicator")).toHaveCount(0);
    await expect(
      settings.getByRole("button", { name: "Change servers, 1 selected" }),
    ).toBeVisible();
    await page.artifact(`server-settings-phone-${theme}`);
  });
}
