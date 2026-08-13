import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

test("settings expose live controls and lock run construction inputs", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');

  await expect(
    settings.getByText("Ready", { exact: true }).first(),
  ).toBeVisible();
  await expect(
    settings.getByLabel("Maximum H1 streams per direction"),
  ).toBeVisible();

  const advanced = settings.locator("summary", {
    hasText: "Customize the compensation model",
  });
  await advanced.focus();
  await advanced.press("Enter");
  await expect(settings.getByLabel("Connection profile")).toBeVisible();

  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Settings"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);

  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(page.getByRole("button", { name: "Abort test" })).toBeVisible();
  await expect(settings.getByRole("button", { name: "short" })).toBeEnabled();
  await expect(settings.getByLabel("Finish stable stages early")).toBeEnabled();
  await expect(settings.getByLabel("Unloaded ping cadence")).toBeDisabled();
  await expect(
    settings.getByLabel("Maximum H1 streams per direction"),
  ).toBeDisabled();
  await expect(
    settings.locator('input[name="throughput-target"]:checked'),
  ).toBeDisabled();
});

// The datagram card is gated on its experimental setting, but a card already
// selected must not vanish under the user — and the note that its number is not
// a speed test belongs to that selection, not to the toggle. Read linearly the
// note is textually indistinguishable from the hints around it unless it
// announces itself, so a screen reader is told it appeared. Sighted readers get
// it above the toggle rather than below, where the end of the scroll hides it.
test("the datagram card follows its selection and announces its caution", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await expect(
    settings.getByText("Ready", { exact: true }).first(),
  ).toBeVisible();

  const card = settings.locator("label", {
    hasText: "WebTransport datagrams",
  });
  const caution = settings
    .getByRole("status")
    .filter({ hasText: "not link speed" });
  const toggle = settings.getByText("Datagram throughput (experimental)");

  await expect(card).toHaveCount(0);
  await expect(caution).toHaveCount(0);

  await toggle.click();
  await expect(card).toHaveCount(1);
  await expect(caution).toContainText("Datagrams are never resent");

  await card.click();
  await expect(card.locator("input")).toBeChecked();

  // Turning the setting off leaves the selection standing, so the card and its
  // caution both stay: a run is still about to happen over datagrams.
  await toggle.click();
  await expect(card.locator("input")).toBeChecked();
  await expect(caution).toBeVisible();

  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Settings"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);
});

test("endpoint summary and diagnostics use accessible disclosure", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
  await page.getByRole("button", { name: "Toggle endpoint info" }).click();
  const endpoint = page.locator('[aria-label="Endpoint info"]');

  await expect(
    endpoint.getByText("Fetch stream · HTTP/1.1 · clear"),
  ).toBeVisible();
  await expect(
    endpoint.getByText(
      "Fetch streams · WebTransport streams · WebTransport datagrams",
    ),
  ).toBeVisible();
  await expect(endpoint.getByText("WebSocket", { exact: true })).toBeVisible();
  await expect(
    endpoint.getByText("Fetch stream over HTTP/1.1 · clear"),
  ).toBeVisible();
  const summary = endpoint.locator("summary", { hasText: "Diagnostics" });
  await summary.focus();
  await summary.press("Enter");
  await expect(
    endpoint.getByText("Server instance", { exact: true }),
  ).toBeVisible();
  await expect(endpoint.locator(".diagnostic-note")).toContainText(
    "changes when the backend restarts",
  );
  await expect(endpoint.getByText("Throughput origin")).toBeVisible();
  await expect(endpoint.getByText("Latency origin")).toBeVisible();
  await expect(
    endpoint.getByRole("button", { name: "Copy diagnostic report" }),
  ).toBeVisible();

  const accessibility = await new AxeBuilder({ page })
    .include('[aria-label="Endpoint info"]')
    .analyze();
  expect(accessibility.violations).toEqual([]);
});
