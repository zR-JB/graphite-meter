import type { Page } from "./webview";
import {
  expect,
  expectVisible,
  openApp,
  openEndpointInfo,
  openSettings,
  test,
} from "./webview";
/* These tests exercise real-runner path resolution, degradation, and refusal with preflight/probe stubs. */
const PROBE = {
  clientIp: "127.0.0.1",
  clientIpVersion: 4,
  clientIpSource: "socket",
  protocolNegotiated: "http/1.1",
};
/** Unreachable latency origin whose aborted probe fails that role independently. */
const DEAD_LATENCY_ORIGIN = "http://127.0.0.1:4199";
function persistConfig(latency: boolean) {
  return JSON.stringify({
    config: {
      stages: {
        latency,
        download: true,
        upload: false,
        bidirectional: false,
      },
      skipLoadedLatencyWhenStageOff: true,
      transports: { throughputTarget: "auto", latencyTarget: "auto" },
    },
  });
}
async function stubPreflight(
  page: Page,
  latency: { baseUrl: string; transport: "websocket" | "webtransport" }[],
) {
  await page.route("**/preflight?*", async (route) => {
    await route.fulfill({
      json: {
        server: { name: "paths-test" },
        engineVersion: "test",
        generation: "paths-generation",
        capabilities: {
          throughput: [
            { baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" },
            { baseUrl: ".", transport: "webtransport", protocol: "http3" },
          ],
          latency,
        },
      },
    });
  });
}
async function preparePaths(
  page: Page,
  latency: { baseUrl: string; transport: "websocket" | "webtransport" }[],
) {
  await page.addInitScript(
    (value) => localStorage.setItem("graphite-meter:v1", value),
    persistConfig(latency.length > 0),
  );
  await stubPreflight(page, latency);
  await page.route("**/probe?*", (route) => route.fulfill({ json: PROBE }));
  await openApp(page, "real");
  return openSettings(page);
}
/** One row of the diagnostics disclosure, by its term. */
function diagnosticRow(page: Page, term: string) {
  return page
    .locator('[aria-label="Endpoint info"] .diagnostics dl > div')
    .filter({ has: page.locator("dt", { hasText: new RegExp(`^${term}$`) }) })
    .locator("dd");
}
async function openDiagnostics(page: Page) {
  const endpoint = await openEndpointInfo(page);
  const summary = endpoint.locator("summary", { hasText: "Diagnostics" });
  await summary.focus();
  await summary.press("Enter");
  return endpoint;
}
test("the diagnostics rows agree with the path card above them", async ({
  page,
}) => {
  const settings = await preparePaths(page, []);
  test.skip(
    await page.evaluate(() => typeof WebTransport === "undefined"),
    "the WebTransport card is disabled without the browser API",
  );
  await expectVisible(settings.getByText("Ready", { exact: true }).first());
  await settings.locator("label", { hasText: "WebTransport · HTTP/3" }).click();
  await expectVisible(settings.getByText("Failed", { exact: true }).first());
  const endpoint = await openDiagnostics(page);
  await expect(
    endpoint.locator(".path").filter({ hasText: "throughput path" }),
  ).toContainText("WebTransport streams");
  await expect(endpoint.locator(".path").first()).toContainText(
    "Path evidence",
  );
  await expect(diagnosticRow(page, "Transports")).toHaveCount(0);
  await expect(diagnosticRow(page, "Streams")).toHaveText(
    "Automatic · 1 continuous stream per direction",
  );
  await page.evaluate(() => {
    if (navigator.clipboard) {
      navigator.clipboard.writeText = () =>
        Promise.reject(new Error("clipboard denied"));
    } else {
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: {
          writeText: () => Promise.reject(new Error("clipboard denied")),
        },
      });
    }
  });
  const errorStart = page.errors.length;
  const consoleStart = page.console.length;
  await endpoint
    .getByRole("button", { name: "Copy diagnostic report" })
    .click();
  await expect(endpoint.locator(".sr-status")).toHaveText(
    "Unable to copy diagnostic report",
  );
  expect(page.errors.slice(errorStart)).toEqual([]);
  expect(
    page.console
      .slice(consoleStart)
      .filter((entry) => entry.startsWith("error:")),
  ).toEqual([]);
});
test("each path's Retry names the path it retries", async ({ page }) => {
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const settings = await preparePaths(page, [
    { baseUrl: DEAD_LATENCY_ORIGIN, transport: "websocket" },
  ]);
  await page.route(`${DEAD_LATENCY_ORIGIN}/**`, (route) => route.abort());
  await expectVisible(
    settings.getByRole("button", { name: "Retry Latency path" }),
  );
  await expectVisible(
    settings.getByRole("button", { name: "Retry Throughput path" }),
  );
  await expect(settings.getByRole("button", { name: /^Retry/ })).toHaveCount(2);
  const consoleStart = page.console.length;
  await settings.getByRole("button", { name: "Retry Latency path" }).click();
  await expectVisible(
    settings
      .locator("fieldset", { hasText: "Latency path" })
      .getByText("Connection check failed", { exact: true }),
  );
  expect(pageErrors).toEqual([]);
  expect(
    page.console
      .slice(consoleStart)
      .filter((entry) => entry.startsWith("error:")),
  ).toEqual([]);
});
test("a shared HTTP preflight failure fails both paths and Retry stays quiet", async ({
  page,
}) => {
  let reachable = true;
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  await page.addInitScript(
    (value) => localStorage.setItem("graphite-meter:v1", value),
    persistConfig(true),
  );
  await page.route("**/preflight?*", async (route) => {
    if (!reachable) {
      await route.abort();
      return;
    }
    await route.fulfill({
      json: {
        server: { name: "paths-test" },
        engineVersion: "test",
        generation: "paths-generation",
        capabilities: {
          throughput: [
            { baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" },
            { baseUrl: ".", transport: "websocket", protocol: "http1" },
          ],
          latency: [{ baseUrl: ".", transport: "websocket" }],
        },
      },
    });
  });
  await page.route("**/probe?*", (route) => route.fulfill({ json: PROBE }));
  await openApp(page, "real");
  const settings = await openSettings(page);
  reachable = false;
  const consoleStart = page.console.length;
  await settings.getByRole("button", { name: "Retry Throughput path" }).click();
  await expect(
    settings.getByText("Connection check failed", { exact: true }),
  ).toHaveCount(2);
  await expect(settings.getByText("Failed", { exact: true })).toHaveCount(2);
  expect(pageErrors).toEqual([]);
  expect(
    page.console
      .slice(consoleStart)
      .filter((entry) => entry.startsWith("error:")),
  ).toEqual([]);
});
test("the readiness badge names a failure over a check in flight", async ({
  page,
}) => {
  let holdProbe = false;
  await page.addInitScript(
    (value) => localStorage.setItem("graphite-meter:v1", value),
    persistConfig(true),
  );
  await stubPreflight(page, [
    { baseUrl: DEAD_LATENCY_ORIGIN, transport: "websocket" },
  ]);
  await page.route("**/probe?*", async (route) => {
    if (holdProbe) return new Promise(() => {});
    await route.fulfill({ json: PROBE });
  });
  await page.route(`${DEAD_LATENCY_ORIGIN}/**`, (route) => route.abort());
  await openApp(page, "real");
  const settings = await openSettings(page);
  const badge = settings.locator(".readiness-badge");
  await expectVisible(
    settings.getByRole("button", { name: "Retry Latency path" }),
  );
  await expect(badge).toHaveText("Path failed");
  holdProbe = true;
  await settings.getByRole("button", { name: "Retry Throughput path" }).click();
  await expect(settings.getByText("Checking", { exact: true })).toBeVisible();
  await expect(badge).toHaveText("Path failed");
});
test("a delayed stale start can be cancelled and the idle view continues", async ({
  page,
}) => {
  await page.addInitScript(
    (value) => localStorage.setItem("graphite-meter:v1", value),
    persistConfig(false),
  );
  await page.route("**/preflight?*", async (route) => {
    await Bun.sleep(300);
    await route.fulfill({
      json: {
        server: { name: "delayed-start" },
        engineVersion: "test",
        generation: "delayed-generation",
        capabilities: {
          throughput: [
            { baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" },
          ],
          latency: [],
        },
      },
    });
  });
  await page.route("**/probe?*", async (route) => {
    await Bun.sleep(300);
    await route.fulfill({ json: PROBE });
  });
  await openApp(page, "real");
  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expectVisible(page.getByRole("button", { name: "Cancel" }));
  await expectVisible(page.getByText("Checking paths", { exact: true }));
  const live = page.locator('output[aria-live="polite"]');
  await expect(live).toContainText("Starting test");
  await expect(live).toContainText("Throughput path");
  await expect(live).toContainText("Latency path");
  await page.keyboard.press("Escape");
  await expectVisible(
    page.getByRole("button", { name: "Start the speed test" }),
  );
  await expect(page.locator('[role="alert"]')).toHaveCount(0);
  await Bun.sleep(500);
  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expectVisible(page.getByRole("button", { name: "Abort test" }));
  expect(await page.locator("#console").getAttribute("data-phase")).not.toBe(
    "idle",
  );
  await page.getByRole("button", { name: "Abort test" }).click();
  await expectVisible(page.getByRole("button", { name: "Run the test again" }));
});
test("a failed start names the affected path in the gauge", async ({
  page,
}) => {
  await page.addInitScript(
    (value) => localStorage.setItem("graphite-meter:v1", value),
    persistConfig(false),
  );
  await stubPreflight(page, []);
  await page.route("**/probe?*", (route) => route.abort());
  await openApp(page, "real");
  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(
    page.locator(".gauge-panel").getByText("Connection check failed", {
      exact: true,
    }),
  ).toBeVisible();
  await expectVisible(
    page.getByText("Throughput path is unavailable", { exact: true }),
  );
  await expect(
    page.locator(".gauge-panel").getByText("Connection check failed", {
      exact: true,
    }),
  ).toHaveCount(1);
  await expect(page.locator(".run-error")).toHaveCount(0);
});
test("the occupancy row reports slots and cautions only past half", async ({
  page,
}) => {
  let load: { active: number; max: number } | null = { active: 1, max: 2 };
  await page.addInitScript(
    (value) => localStorage.setItem("graphite-meter:v1", value),
    persistConfig(false),
  );
  await stubPreflight(page, []);
  await page.route("**/probe?*", (route) =>
    route.fulfill({ json: load ? { ...PROBE, load } : PROBE }),
  );
  await page.goto("/?engine=real");
  await openDiagnostics(page);
  const row = diagnosticRow(page, "Server load");
  await expect(row).toHaveText("1 of 2 slots");
  load = { active: 3, max: 4 };
  await page.reload();
  await openDiagnostics(page);
  await expect(row).toHaveText(
    "3 of 4 slots · server busy — results may be affected",
  );
  load = { active: 0, max: 0 };
  await page.reload();
  await openDiagnostics(page);
  await expect(row).toHaveCount(0);
  load = null;
  await page.reload();
  await openDiagnostics(page);
  await expect(row).toHaveCount(0);
});
