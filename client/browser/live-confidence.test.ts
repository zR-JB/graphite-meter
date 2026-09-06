import {
  expect,
  expectVisible,
  prepareApp,
  startTest,
  test,
  waitForCompletion,
} from "./webview";
test("live results stay measurement-first and defer confidence verdicts", async ({
  page,
}) => {
  const settings = await prepareApp(page, "live-confidence");
  await expectVisible(settings.getByText("Ready", { exact: true }).first());
  await settings.getByText("Include concurrent download + upload").click();
  await settings.getByLabel("Bidirectional ms").fill("8000");
  await startTest(page);
  await expect(page.locator(".gauge-value")).toHaveAttribute(
    "aria-hidden",
    "true",
  );
  const bidirectional = page.locator(".result-chip", { hasText: "Bi-dir" });
  await expectVisible(bidirectional);
  await expect(bidirectional.locator(".chip-val")).toHaveAttribute(
    "aria-hidden",
    "true",
  );
  await expect(bidirectional.locator(".sr-only")).toContainText("Bi-dir:");
  await expect(bidirectional.locator(".pip")).toHaveCount(0);
  await expect(bidirectional).not.toContainText(/wire/i);
  await expect(bidirectional.locator(".chip-val .num")).not.toHaveText("—");
  await waitForCompletion(page, 10_000);
  const completed = page.locator(".result-card", { hasText: "Bi-dir" });
  await expect(completed.locator(".pip")).toHaveText("high");
});
