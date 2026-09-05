import { AxeBuilder } from "./webview";
import {
  expect,
  expectVisible,
  openApp,
  openEndpointInfo,
  test,
} from "./webview";
test("Endpoint Info opens the accessible generated legal modal", async ({
  page,
}) => {
  let fetches = 0;
  await page.route("**/legal/about.json", () => {
    fetches += 1;
  });
  await openApp(page);
  const endpoint = await openEndpointInfo(page);
  await expectVisible(endpoint.getByRole("button", { name: "About & legal" }));
  await expect(endpoint.locator('a[href*="gnu.org"]')).toHaveCount(0);
  await endpoint.getByRole("button", { name: "About & legal" }).click();
  const dialog = page.getByRole("dialog", { name: "About & legal" });
  await expectVisible(dialog);
  await expect(dialog).toHaveAttribute("aria-modal", "true");
  await expect(dialog).toContainText("Graphite Meter");
  await expect(dialog).toContainText("Copyright © 2026 zR-JB");
  await expect(dialog).toContainText("AGPL-3.0-or-later");
  await expect(
    dialog.getByRole("link", { name: "Source code" }),
  ).toHaveAttribute("href", "https://github.com/zR-JB/graphite-meter");
  await expect(
    dialog.getByRole("link", { name: "Project license" }),
  ).toHaveAttribute("href", "legal/LICENSE.txt");
  const noticesLink = dialog.getByRole("link", {
    name: "Third-party notices",
  });
  await expect(noticesLink).toHaveAttribute(
    "href",
    "legal/THIRD_PARTY_NOTICES.txt",
  );
  await expect(noticesLink).toHaveAttribute("target", "_blank");
  await expect(noticesLink).toHaveAttribute("rel", "noopener noreferrer");
  await expect(dialog).toContainText("github.com/coder/websocket");
  await expect(dialog).toContainText("svelte (npm)");
  await expect(dialog.locator(".component details").first()).toHaveAttribute(
    "open",
    "",
  );
  await expect(dialog.locator(".component details:not([open])")).toHaveCount(0);
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
  expect(await page.evaluate(() => document.body.style.overflow)).toBe("");
  await expect(
    endpoint.getByRole("button", { name: "About & legal" }),
  ).toBeFocused();
  await endpoint.getByRole("button", { name: "About & legal" }).click();
  await expectVisible(page.getByRole("dialog", { name: "About & legal" }));
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
  });
  await openApp(page);
  await openEndpointInfo(page);
  await page.getByRole("button", { name: "About & legal" }).click();
  const dialog = page.getByRole("dialog", { name: "About & legal" });
  await expect(dialog).toContainText("Unable to load legal notices.");
  await dialog.getByRole("button", { name: "Retry" }).click();
  await expect(dialog).toContainText("Third-party software");
  expect(attempts).toBe(2);
});
test("legal modal remains stable at a narrow reduced-motion viewport", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openApp(page, "dummy", { width: 390, height: 844 });
  await openEndpointInfo(page);
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

test("legal focus trap includes the final collapsed disclosure", async ({
  page,
}) => {
  await openApp(page);
  const endpoint = await openEndpointInfo(page);
  await endpoint.getByRole("button", { name: "About & legal" }).click();
  const dialog = page.getByRole("dialog", { name: "About & legal" });
  await expect(dialog).toContainText("Third-party software");
  const lastDetails = dialog.locator(".component:last-child details");
  const lastSummary = lastDetails.locator("summary");
  const close = dialog.getByRole("button", { name: "Close" });
  await lastSummary.click();
  await expect(
    dialog.locator(".component:last-child details[open]"),
  ).toHaveCount(0);

  await dialog.locator(".component:nth-last-child(2) a").focus();
  await page.keyboard.press("Tab");
  await expect(lastSummary).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(close).toBeFocused();
  await page.keyboard.press("Shift+Tab");
  await expect(lastSummary).toBeFocused();

  await page.keyboard.press("Enter");
  await expect(lastDetails).toHaveAttribute("open", "");
  await page.keyboard.press("Tab");
  await expect(lastDetails.locator("a")).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(close).toBeFocused();
});
