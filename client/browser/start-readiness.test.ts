import { expect, openApp, openSettings, startTest, test } from "./webview";

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

test("a missing saved peer explains why an early start was refused", async ({
  page,
}) => {
  await page.addInitScript(() =>
    localStorage.setItem(
      "graphite-meter:server-selection:v1",
      JSON.stringify([
        { id: "self", url: location.origin },
        { id: "removed", url: "http://localhost:45679" },
      ]),
    ),
  );
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
  const settings = await openSettings(page);
  await expect(settings).toContainText("The saved selection has changed.");
  await page.keyboard.press("Escape");
  await startTest(page);
  await expect(page.locator(".gauge-panel")).toContainText("Test cannot start");
  await expect(page.locator(".gauge-panel")).toContainText(
    "The saved selection changed. Open Settings to choose the servers to test.",
  );
});

test("Start checks a selected peer and reports its failed path", async ({
  page,
}) => {
  await page.addInitScript(() =>
    localStorage.setItem(
      "graphite-meter:v1",
      JSON.stringify({
        config: {
          stages: {
            latency: false,
            download: true,
            upload: false,
            bidirectional: false,
          },
          skipLoadedLatencyWhenStageOff: true,
        },
      }),
    ),
  );
  let peerChecks = 0;
  const peer = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      if (new URL(request.url).pathname === "/preflight") peerChecks++;
      return new Response("Temporarily unavailable", {
        status: 503,
        headers: { "Access-Control-Allow-Origin": "*" },
      });
    },
  });
  try {
    await page.route("**/servers", (route) =>
      route.fulfill({
        json: {
          defaultSelection: ["peer"],
          servers: [
            { id: "self", url: ".", name: "Home" },
            { id: "peer", url: peer.url.origin, name: "Peer" },
          ],
        },
      }),
    );
    await openApp(page, "real");
    const settings = await openSettings(page);
    await expect(settings.getByRole("checkbox").nth(1)).toBeChecked();
    await page.keyboard.press("Escape");
    await startTest(page);
    await expect(page.locator(".gauge-panel")).toContainText(
      "Connection check failed",
    );
    expect(peerChecks).toBeGreaterThan(0);
    await expect(page.locator(".gauge-panel")).toContainText(
      "Throughput path is unavailable",
    );
  } finally {
    peer.stop(true);
  }
});
