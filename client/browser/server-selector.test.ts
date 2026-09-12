import { test, expect, openApp, openSettings, AxeBuilder } from "./webview";

test("server listbox keeps selection on cancel, supports typeahead, and fits the viewport", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.route("**/servers", (route) =>
    route.fulfill({
      json: {
        defaultSelection: ["self", "peer"],
        servers: [
          { id: "self", url: ".", name: "Home" },
          { id: "peer", url: "http://localhost:45678", name: "Frankfurt" },
        ],
      },
    }),
  );
  await openApp(page, "real", { width: 390, height: 740 });
  await openSettings(page);
  const selector = page.getByRole("combobox", {
    name: "Latency measurement servers",
  });
  const menu = page.getByRole("listbox", {
    name: "Latency measurement servers",
  });
  await selector.click();
  await expect(menu).toBeVisible();
  await expect(selector).toBeFocused();
  await selector.evaluate((button) => {
    document
      .getElementById(button.getAttribute("aria-controls")!)!
      .hidePopover();
    button.click();
  });
  await expect(menu).toBeVisible();
  const previous = await selector.getAttribute("value");
  await selector.press("End");
  await selector.press("Escape");
  await expect(menu).not.toBeVisible();
  await expect(selector).toBeFocused();
  await expect(selector).toHaveValue(previous!);
  await selector.press("f");
  await expect(menu).toBeVisible();
  await selector.press("Enter");
  await expect(selector).toHaveValue("peer");
  await selector.click();
  await expect(page.getByRole("option", { name: "Frankfurt" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  expect(
    (
      await new AxeBuilder({ page })
        .include('[aria-label="Settings"]')
        .analyze()
    ).violations,
  ).toEqual([]);
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 740 });
    await expect
      .poll(() =>
        menu.evaluate((element) => {
          const box = element.getBoundingClientRect();
          return (
            box.left >= 0 &&
            box.right <= innerWidth &&
            box.top >= 0 &&
            box.bottom <= innerHeight
          );
        }),
      )
      .toBe(true);
  }
  await page.artifact("compact-server-listbox");
  await selector.press("Tab");
  await expect(menu).not.toBeVisible();
  await expect(selector).not.toBeFocused();
});
