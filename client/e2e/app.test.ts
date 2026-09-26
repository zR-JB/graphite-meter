import { isHistoryRecord } from "../src/lib/history/types";
import {
  baseConfig,
  home,
  open,
  openSettings,
  phase,
  closeSettings,
  ready,
  run,
  runButton,
  savedResult,
} from "./fleet";
import { seriousViolations, expect, test } from "./webview";

test("an HTTP/1.1 and WebSocket run is saved and listed after reload", async (page) => {
  const version = await fetch(`${home.http}/version.json`);
  expect((await version.json()).label).toBe("prod");
  await open(page, home.http, {
    config: {
      transports: {
        throughputTarget: "protocol:http1",
        latencyTarget: "transport:websocket",
      },
    },
  });
  await ready(page);
  const saved = await run(page);
  expect(isHistoryRecord(saved)).toBe(true);
  expect(saved.outcome).toBe("complete");
  expect(saved.multiServer?.failures).toEqual([]);
  const preflight = await fetch(`${home.http}/preflight`);
  const identity = await preflight.json();
  expect(saved.server.name).toBe(identity.server.name);
  expect(saved.server.engine).toBe(identity.engineVersion);
  for (const stage of ["latency", "download", "upload"] as const)
    expect(saved.stages[stage].status).toBe("complete");
  expect(saved.stages.latency.lanes.latency?.count).toBeGreaterThan(0);
  expect(saved.stages.download.result?.reportedBytesPerSec).toBeGreaterThan(0);
  expect(saved.transport.throughput.kind).toBe("fetch-stream");
  expect(saved.transport.latency.kind).toBe("websocket");
  expect(saved.multiServer?.servers[0].throughput?.origin).toBe(home.http);

  const row = page.locator(`a.result-row[data-history-id="${saved.id}"]`);
  await page.getByRole("button", { name: "Open History" }).click();
  await expect(page.getByRole("heading", { name: "History" })).toBeVisible();
  await expect(row).toHaveCount(1);
  await page.reload();
  await expect(row).toHaveCount(1);
});

test("stopping during download freezes elapsed time and a rerun completes", async (page) => {
  await open(page, home.url, {
    config: { duration: { ...baseConfig.duration, downloadMs: 1500 } },
  });
  await ready(page);
  await runButton(page, "Start test").click();
  await expect(phase(page, "download")).toHaveCount(1, { timeout: 10_000 });
  await runButton(page, "Stop test").click();
  await expect(phase(page, "aborted")).toHaveCount(1);
  const elapsed = page.locator(".elapsed");
  const frozen = await elapsed.textContent();
  await Bun.sleep(600);
  expect(await elapsed.textContent()).toBe(frozen);
  const saved = await run(page);
  expect(saved.outcome).toBe("complete");
});

test("a run in a hidden tab completes and saves", async (page) => {
  await open(page);
  await ready(page);
  const { windowId } = await page.cdp("Browser.getWindowForTarget");
  const bounds = (windowState: string) =>
    page.cdp("Browser.setWindowBounds", { windowId, bounds: { windowState } });
  const startedAt = Date.now();
  await runButton(page, "Start test").click();
  await bounds("minimized");
  expect(await page.evaluate(() => document.visibilityState)).toBe("hidden");
  const saved = await savedResult(page, startedAt, 20_000);
  await bounds("normal");
  expect(saved.outcome).toBe("complete");
  await expect(phase(page, "complete")).toHaveCount(1);
});

const viewports = [
  [390, 844],
  [768, 1024],
  [1280, 800],
  [1920, 1080],
] as const;

test("a completed run fits every layout and theme without serious violations", async (page) => {
  await open(page);
  await ready(page);
  await run(page);
  // The completion toast fades on a timer; judge contrast after it has gone.
  await expect(page.locator(".phase-toast.visible")).toHaveCount(0, {
    timeout: 10_000,
  });
  for (const [width, height] of viewports)
    for (const scheme of ["light", "dark"]) {
      await page.setViewportSize({ width, height });
      await page.cdp("Emulation.setEmulatedMedia", {
        features: [{ name: "prefers-color-scheme", value: scheme }],
      });
      await openSettings(page);
      const layout = () =>
        page.evaluate(() => {
          const overflow = [
            ...document.querySelectorAll(".panel-body, .stage"),
          ].filter((el) => el.scrollWidth > el.clientWidth + 1);
          const stage = document
            .querySelector(".gauge-panel .stage")!
            .getBoundingClientRect();
          const gauge = document
            .querySelector(".gauge-face")!
            .getBoundingClientRect();
          return {
            width: innerWidth,
            page: document.documentElement.scrollWidth <= innerWidth,
            overflow: overflow.map((el) => el.className),
            gauge:
              gauge.left >= stage.left - 1 &&
              gauge.right <= stage.right + 1 &&
              gauge.top >= stage.top - 1 &&
              gauge.bottom <= stage.bottom + 1,
          };
        });
      await expect
        .poll(layout)
        .toEqual({ width, page: true, overflow: [], gauge: true });
      expect(await seriousViolations(page)).toEqual([]);
      await closeSettings(page);
    }
  await page.getByRole("button", { name: "Details" }).click();
  const about = page.getByRole("button", { name: "About & legal" });
  await about.click();
  const dialog = page.getByRole("dialog", { name: "About & legal" });
  expect(await dialog.evaluate((el) => el.matches(":modal"))).toBe(true);
  await expect(dialog).toContainText("AGPL-3.0-or-later");
  expect(await seriousViolations(page, '[role="dialog"]')).toEqual([]);
  await page.raw.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(about).toBeFocused();
});
