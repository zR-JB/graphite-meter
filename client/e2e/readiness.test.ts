import {
  baseConfig,
  catalog,
  closeSettings,
  home,
  open,
  openSettings,
  phase,
  ready,
  run,
  runButton,
  spawnPeer,
} from "./fleet";
import { expect, test, type Page } from "./webview";

const footer = (page: Page) => page.locator("footer.status .label");

test("a verified peer that dies fails its idle recheck, and Start leaves it out", async (page) => {
  const oslo = await spawnPeer("Oslo");
  const bergen = await spawnPeer("Bergen", catalog(oslo.server));
  try {
    await open(page, bergen.server.url, {
      servers: [{ id: "self", url: bergen.server.url }, oslo.server],
    });
    await ready(page);
    oslo.kill("SIGKILL");
    const settings = await openSettings(page);
    await expect(
      settings.locator(`.server-status[data-state="failed"]`),
    ).toHaveCount(1, { timeout: 10_000 });
    await closeSettings(page);
    const { multiServer } = (await run(page)).result;
    expect(multiServer.participants).toEqual(["self"]);
    expect(multiServer.failures).toMatchObject([
      { serverId: "oslo", reason: "preparation-failed" },
    ]);
  } finally {
    oslo.kill();
    bergen.kill();
  }
});

test("a stream plan that cannot fit shows its reason before Start", async (page) => {
  await open(page, home.url, {
    config: { transferStreams: { mode: "forced", count: 12 } },
  });
  const settings = await openSettings(page);
  await expect(settings.locator('[data-readiness="blocked"]')).toBeVisible({
    timeout: 15_000,
  });
  const reason = `Forced streams would occupy the progress and control capacity of ${home.name}.`;
  await expect(settings.locator(".notice")).toContainText(reason);
  await closeSettings(page);
  await expect(page.locator(".gauge-hint")).toContainText(reason);
  await runButton(page, "Start test").click();
  await expect(footer(page)).toHaveText("Test cannot start");
  await expect(phase(page, "idle")).toHaveCount(1);
});

test("without idle latency the page settles Connected and never shows a blocker while loading", async (page) => {
  await page.addInitScript(() => {
    const seen: string[] = ((window as any).__labels = []);
    new MutationObserver(() => {
      const label = document.querySelector("footer.status .label");
      if (label?.textContent && seen.at(-1) !== label.textContent)
        seen.push(label.textContent);
    }).observe(document, { subtree: true, childList: true });
  });
  await open(page, home.url, {
    config: { stages: { ...baseConfig.stages, latency: false } },
  });
  await expect(page.locator('.pulse .status-dot[data-tone="ok"]')).toBeVisible({
    timeout: 15_000,
  });
  expect(await page.evaluate(() => (window as any).__labels)).toEqual([
    "Not started",
  ]);
});

test("cancelling a new start keeps the previous result", async (page) => {
  await page.addInitScript(() => {
    const original = window.fetch.bind(window);
    window.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const held = (window as { holdPreflight?: boolean }).holdPreflight;
      if (
        held &&
        new URL(String(input), location.href).pathname === "/preflight"
      )
        await new Promise((resolve) => setTimeout(resolve, 5_000));
      return original(input, init);
    }) as typeof fetch;
  });
  await open(page);
  await ready(page);
  await run(page);
  await page.evaluate(
    () => void Object.assign(window, { holdPreflight: true }),
  );
  await runButton(page, "Run again").click();
  await runButton(page, "Cancel").click();
  await expect(phase(page, "complete")).toHaveCount(1);
  await expect(page.locator(".result-cards")).toBeVisible();
});
