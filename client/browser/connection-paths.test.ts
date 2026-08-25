import type { Page } from "./webview";
import { expect, test } from "./webview";

/* The connection panel and the endpoint drawer describe the same two paths.
 * These run against `?engine=real` with /preflight and /probe stubbed, because
 * only the real runner resolves, degrades, and refuses a path — the dummy
 * accepts every selection, so none of these states exist under it. */

const PROBE = {
  clientIp: "127.0.0.1",
  clientIpVersion: 4,
  clientIpSource: "socket",
  protocolNegotiated: "http/1.1",
};

/** A latency origin nothing listens on: its /probe is aborted, so the latency
 *  role fails on its own without the throughput one failing with it. */
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

/** One row of the diagnostics disclosure, by its term. */
function diagnosticRow(page: Page, term: string) {
  return page
    .locator('[aria-label="Endpoint info"] .diagnostics dl > div')
    .filter({ has: page.locator("dt", { hasText: new RegExp(`^${term}$`) }) })
    .locator("dd");
}

async function openDiagnostics(page: Page) {
  await page.getByRole("button", { name: "Toggle endpoint info" }).click();
  const endpoint = page.locator('[aria-label="Endpoint info"]');
  const summary = endpoint.locator("summary", { hasText: "Diagnostics" });
  await summary.focus();
  await summary.press("Enter");
  return endpoint;
}

// The drawer's own rows read the run's last evidence, which outlives the
// selection that produced it. The card above then names one path while the rows
// below name another, and the disagreement is permanent whenever the re-check
// fails — which is the ordinary reason a user changes cards.
test("the diagnostics rows agree with the path card above them", async ({
  page,
}) => {
  await page.addInitScript(
    (value) => localStorage.setItem("graphite-meter:v1", value),
    persistConfig(false),
  );
  await stubPreflight(page, []);
  await page.route("**/probe?*", (route) => route.fulfill({ json: PROBE }));

  await page.goto("/?engine=real");
  // WebTransport is secure-context only, so this is asked of the page itself.
  test.skip(
    await page.evaluate(() => typeof WebTransport === "undefined"),
    "the WebTransport card is disabled without the browser API",
  );
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await expect(
    settings.getByText("Ready", { exact: true }).first(),
  ).toBeVisible();

  // The session card is advertised and the browser can drive it, but nothing
  // answers UDP here, so the explicit selection fails its role loudly.
  await settings.locator("label", { hasText: "WebTransport · HTTP/3" }).click();
  await expect(
    settings.getByText("Failed", { exact: true }).first(),
  ).toBeVisible();

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

// Two pickers mount at once and each renders its own Retry. A <legend> does not
// contribute to a descendant button's accessible name, so without one of their
// own the button rotor reads "Retry, Retry".
test("each path's Retry names the path it retries", async ({ page }) => {
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  await page.addInitScript(
    (value) => localStorage.setItem("graphite-meter:v1", value),
    persistConfig(true),
  );
  await stubPreflight(page, [
    { baseUrl: DEAD_LATENCY_ORIGIN, transport: "websocket" },
  ]);
  await page.route("**/probe?*", (route) => route.fulfill({ json: PROBE }));
  await page.route(`${DEAD_LATENCY_ORIGIN}/**`, (route) => route.abort());

  await page.goto("/?engine=real");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');

  await expect(
    settings.getByRole("button", { name: "Retry Latency path" }),
  ).toBeVisible();
  await expect(
    settings.getByRole("button", { name: "Retry Throughput path" }),
  ).toBeVisible();
  await expect(settings.getByRole("button", { name: /^Retry/ })).toHaveCount(2);

  const consoleStart = page.console.length;
  await settings.getByRole("button", { name: "Retry Latency path" }).click();
  await expect(
    settings
      .locator("fieldset", { hasText: "Latency path" })
      .getByText("Connection check failed", { exact: true }),
  ).toBeVisible();
  expect(pageErrors).toEqual([]);
  expect(
    page.console
      .slice(consoleStart)
      .filter((entry) => entry.startsWith("error:")),
  ).toEqual([]);
});

test("a shared preflight outage fails both paths and Retry stays quiet", async ({
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

  await page.goto("/?engine=real");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  await expect(settings.getByText("Ready", { exact: true })).toHaveCount(2);

  // Changing to the advertised WebSocket target proves the cards can become
  // Ready again before the shared discovery request is taken away.
  await settings.locator("label", { hasText: "WebSocket" }).click();
  await expect(settings.getByText("Ready", { exact: true })).toHaveCount(2);
  reachable = false;
  const consoleStart = page.console.length;
  await settings.getByRole("button", { name: "Retry Throughput path" }).click();
  await expect(
    settings.getByText("Server could not be reached", { exact: true }),
  ).toHaveCount(2);
  await expect(settings.getByText("Failed", { exact: true })).toHaveCount(2);
  expect(pageErrors).toEqual([]);
  expect(
    page.console
      .slice(consoleStart)
      .filter((entry) => entry.startsWith("error:")),
  ).toEqual([]);
});

// The badge is the one panel-level summary the user is meant to trust.
// Retrying one role while the other is failed must not downgrade the dead path
// to a spinner: a check in flight says the panel may yet come good, and the
// failed path below it will not.
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

  await page.goto("/?engine=real");
  await page.getByRole("button", { name: "Open settings" }).click();
  const settings = page.locator('[aria-label="Settings"]');
  const badge = settings.locator(".readiness-badge");
  await expect(
    settings.getByRole("button", { name: "Retry Latency path" }),
  ).toBeVisible();
  await expect(badge).toHaveText("Path failed");

  // The throughput probe now hangs, so retrying that role alone leaves it in
  // flight while the latency path stays failed underneath.
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
    // Keep boot/path validation in flight long enough for the user-visible
    // preparation state and Escape cancellation to exercise the real seam.
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

  await page.goto("/?engine=real");
  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(page.getByRole("button", { name: "Cancel" })).toBeVisible();
  await expect(page.getByText("Checking paths", { exact: true })).toBeVisible();
  const live = page.locator('output[aria-live="polite"]');
  await expect(live).toContainText("Starting test");
  await expect(live).toContainText("Throughput path");
  await expect(live).toContainText("Latency path");
  await page.keyboard.press("Escape");
  await expect(
    page.getByRole("button", { name: "Start the speed test" }),
  ).toBeVisible();
  await expect(page.locator('[role="alert"]')).toHaveCount(0);
  await Bun.sleep(500);
  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(page.getByRole("button", { name: "Abort test" })).toBeVisible();
  expect(await page.locator("#console").getAttribute("data-phase")).not.toBe(
    "idle",
  );
  await page.getByRole("button", { name: "Abort test" }).click();
  await expect(
    page.getByRole("button", { name: "Run the test again" }),
  ).toBeVisible();
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

  await page.goto("/?engine=real");
  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(
    page.getByText("Connection check failed", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Throughput path is unavailable", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Connection check failed", { exact: true }),
  ).toHaveCount(1);
  await expect(page.locator(".run-error")).toHaveCount(0);
});

// Occupancy is a caution about neighbours, so it fires past half rather than at
// it — one other user of two slots is not a busy server — and a server with no
// measurement slots configured reports no occupancy at all.
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

  // No slots configured is no occupancy to report, not zero of zero.
  load = { active: 0, max: 0 };
  await page.reload();
  await openDiagnostics(page);
  await expect(row).toHaveCount(0);

  load = null;
  await page.reload();
  await openDiagnostics(page);
  await expect(row).toHaveCount(0);
});
