import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

test("Endpoint Info opens the accessible generated legal modal", async ({
  page,
}) => {
  let fetches = 0;
  await page.route("**/legal/about.json", (route) => {
    fetches += 1;
    return route.continue();
  });
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Toggle endpoint info" }).click();

  const endpoint = page.locator('[aria-label="Endpoint info"]');
  await expect(
    endpoint.getByRole("button", { name: "About & legal" }),
  ).toBeVisible();
  await expect(endpoint.locator('a[href*="gnu.org"]')).toHaveCount(0);
  await endpoint.getByRole("button", { name: "About & legal" }).click();

  const dialog = page.getByRole("dialog", { name: "About & legal" });
  await expect(dialog).toBeVisible();
  await expect(dialog).toHaveAttribute("aria-modal", "true");
  await expect(dialog).toContainText("Graphite Meter");
  await expect(dialog).toContainText("Copyright © 2026 zR-JB");
  await expect(dialog).toContainText("AGPL-3.0-or-later");
  await expect(
    dialog.getByRole("link", { name: "Source code" }),
  ).toHaveAttribute("href", "https://github.com/zR-JB/graphite-meter");
  await expect(dialog).toContainText("GNU AFFERO GENERAL PUBLIC LICENSE");
  await expect(dialog).toContainText("github.com/coder/websocket");
  await expect(dialog).toContainText("svelte (npm)");

  const body = dialog.locator(".legal-body");
  expect(
    await body.evaluate((node) => node.scrollHeight > node.clientHeight),
  ).toBe(true);
  expect(await page.evaluate(() => document.body.style.overflow)).toBe(
    "hidden",
  );
  await page.keyboard.press("s");
  await page.keyboard.press("t");
  await expect(page.locator('[aria-label="Settings"]')).toHaveAttribute(
    "inert",
    "",
  );

  const accessibility = await new AxeBuilder({ page })
    .include('[role="dialog"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);

  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(
    endpoint.getByRole("button", { name: "About & legal" }),
  ).toBeFocused();

  await endpoint.getByRole("button", { name: "About & legal" }).click();
  await expect(
    page.getByRole("dialog", { name: "About & legal" }),
  ).toBeVisible();
  expect(fetches).toBe(1);
});

test("legal modal reports load failure and Retry can recover", async ({
  page,
}) => {
  let attempts = 0;
  await page.route("**/legal/about.json", (route) => {
    attempts += 1;
    if (attempts === 1) {
      return route.fulfill({ status: 503, body: "unavailable" });
    }
    return route.continue();
  });
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Toggle endpoint info" }).click();
  await page.getByRole("button", { name: "About & legal" }).click();
  const dialog = page.getByRole("dialog", { name: "About & legal" });
  await expect(dialog).toContainText("Unable to load legal notices.");
  await dialog.getByRole("button", { name: "Retry" }).click();
  await expect(dialog).toContainText("GNU AFFERO GENERAL PUBLIC LICENSE");
  expect(attempts).toBe(2);
});

test("legal modal remains stable at a narrow reduced-motion viewport", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Toggle endpoint info" }).click();
  await page.getByRole("button", { name: "About & legal" }).click();
  const dialog = page.getByRole("dialog", { name: "About & legal" });
  const before = await page.locator(".gauge-panel").boundingBox();
  await dialog.locator(".legal-body").evaluate((node) => {
    node.scrollTop = node.scrollHeight;
  });
  const after = await page.locator(".gauge-panel").boundingBox();
  expect(after?.height).toBe(before?.height);
  expect(
    await dialog.evaluate((node) => node.getBoundingClientRect().height),
  ).toBeLessThanOrEqual(760);
});
