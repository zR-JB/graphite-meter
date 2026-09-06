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
            { id: "self", url: ".", name: "1&1", location: "Berlin" },
            {
              id: "peer",
              url: "http://localhost:45678",
              name: "Vodafone",
              location: "Berlin",
            },
          ],
        },
      }),
    );
    await page.route("**/preflight?*", (route) =>
      route.fulfill({
        json: {
          server: { name: "1&1", location: "Berlin" },
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
    const band = settings.getByRole("group", {
      name: "Servers to test",
      exact: true,
    });
    await expect(band.getByRole("button").first()).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await expect(band.getByRole("button").first()).toHaveText("✓1&1 · Berlin");
    await expect(band.getByRole("button").nth(1)).toHaveText(
      "✓Vodafone · Berlin",
    );
    const appearance = await band.getByRole("button").evaluateAll((buttons) =>
      buttons.map((button) => ({
        accent: button.style.getPropertyValue("--server-accent"),
        left: button.getBoundingClientRect().left,
        right: button.getBoundingClientRect().right,
      })),
    );
    expect(appearance[0].accent).not.toBe(appearance[1].accent);
    expect(appearance[0].right - appearance[1].left).toBeCloseTo(32, 0);
    expect(
      await band.getByRole("button").evaluateAll((buttons) => {
        const next = buttons[1].getBoundingClientRect();
        return [
          document
            .elementFromPoint(next.left + 2, next.top + 2)
            ?.closest("button") === buttons[0],
          document
            .elementFromPoint(next.left + 2, next.top + next.height / 2)
            ?.closest("button") === buttons[1],
        ];
      }),
    ).toEqual([true, true]);
    await band.getByRole("button").first().focus();
    await expect(page.getByRole("tooltip")).toContainText("Berlin");
    await page.keyboard.press("Escape");
    await openSettings(page);
    await expectNoHorizontalOverflow(settings.locator(".panel-body"));
    expect(
      (
        await new AxeBuilder({ page })
          .include('[aria-label="Settings"]')
          .analyze()
      ).violations,
    ).toEqual([]);
    await page.artifact(`server-band-desktop-${theme}`);
    await page.setViewportSize({ width: 320, height: 740 });
    await expectNoHorizontalOverflow(settings.locator(".panel-body"));
    await band.getByRole("button").first().focus();
    await page.keyboard.press("Space");
    await expect(band.getByRole("button").first()).toHaveAttribute(
      "aria-pressed",
      "false",
    );
    expect(
      await band
        .getByRole("button")
        .nth(1)
        .evaluate((button) => button.style.getPropertyValue("--server-accent")),
    ).toBe(appearance[1].accent);
    await expect(page.locator(".server-indicator")).toHaveCount(0);
    await expect(settings.locator(".server-heading")).toContainText("1 / 4");
    await page.artifact(`server-settings-phone-${theme}`);
  });
}
