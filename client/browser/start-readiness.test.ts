import { expect, openApp, startTest, test } from "./webview";

test("starting while the catalogue is loading shows actionable feedback", async ({
  page,
}) => {
  let releaseCatalog!: () => void;
  const catalogReady = new Promise<void>((resolve) => {
    releaseCatalog = resolve;
  });
  await page.route("**/servers", async (route) => {
    await catalogReady;
    await route.fulfill({
      json: {
        defaultSelection: ["self"],
        servers: [{ id: "self", url: ".", name: "Home" }],
      },
    });
  });
  try {
    await openApp(page, "real");
    await startTest(page);
    await expect(page.locator(".gauge-panel")).toContainText(
      "Test cannot start",
    );
    await expect(page.locator(".gauge-panel")).toContainText(
      "Servers are still loading. Try again in a moment.",
    );
  } finally {
    releaseCatalog();
  }
});

test("an unresolved peer selection explains why an early start was refused", async ({
  page,
}) => {
  await page.route("**/servers", (route) =>
    route.fulfill({
      json: {
        defaultSelection: ["self", "peer"],
        servers: [
          { id: "self", url: ".", name: "Home" },
          { id: "peer", url: "http://localhost:45678", name: "Peer" },
        ],
      },
    }),
  );
  await openApp(page, "real");
  await expect(page.locator(".server-indicator")).toHaveText(
    "2 servers selected",
  );
  await startTest(page);
  await expect(page.locator(".gauge-panel")).toContainText("Test cannot start");
  await expect(page.locator(".gauge-panel")).toContainText(
    "Open Settings to resolve the selected servers before starting.",
  );
});
