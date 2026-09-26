import {
  home,
  open,
  openSettings,
  phase,
  ready,
  run,
  runButton,
  spawnPeer,
} from "./fleet";
import type { Server } from "./servers";
import { expect, test, type Page } from "./webview";

const catalog = (...servers: Server[]) => ({
  GM_SERVER_CATALOG: JSON.stringify({
    servers: servers.map(({ id, name, url }) => ({ id, name, url })),
  }),
});

async function regainPage(page: Page) {
  const { windowId } = await page.cdp("Browser.getWindowForTarget");
  for (const windowState of ["minimized", "normal"])
    await page.cdp("Browser.setWindowBounds", {
      windowId,
      bounds: { windowState },
    });
}

test("a verified peer that dies turns Unavailable when the page returns, and Start refuses it", async (page) => {
  const oslo = await spawnPeer("Oslo");
  const bergen = await spawnPeer("Bergen", catalog(oslo.server));
  try {
    await open(page, bergen.server.url, {
      servers: [{ id: "self", url: bergen.server.url }, oslo.server],
    });
    await ready(page);
    oslo.kill("SIGKILL");
    await regainPage(page);
    const settings = await openSettings(page);
    await expect(
      settings.locator(`.server-status[data-state="failed"]`),
    ).toHaveCount(1, { timeout: 10_000 });
    await runButton(page, "Start test").click();
    await Bun.sleep(1_500);
    await expect(phase(page, "idle")).toHaveCount(1);
  } finally {
    oslo.kill();
    bergen.kill();
  }
});

test("a stream plan that cannot fit shows its reason before Start", async (page) => {
  await open(page, home.url, {
    config: { transferStreams: { mode: "forced", count: 12 } },
  });
  await ready(page);
  await expect(page.locator(".gauge-hint")).toContainText("Forced streams");
  await runButton(page, "Start test").click();
  await Bun.sleep(500);
  await expect(phase(page, "idle")).toHaveCount(1);
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
