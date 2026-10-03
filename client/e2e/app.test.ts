import { readHistoryRecord } from "../src/lib/history/types";
import type { LegalAbout } from "../src/lib/legal/types";
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
import { seriousViolations, expect, settled, test } from "./webview";

test("an HTTP/1.1 and WebSocket run is saved and listed after reload", async (page) => {
  const version = await fetch(`${home.http}/version.json`);
  expect((await version.json()).label).toBe("prod");
  await open(page, home.http, {
    config: {
      transports: {
        throughputTarget: "protocol:http1",
        latencyTarget: "transport:websocket",
      },
      stages: { ...baseConfig.stages, bidirectional: true },
    },
  });
  await ready(page);
  const saved = await run(page);
  expect(readHistoryRecord(saved)).toBe(saved);
  expect(saved.result.outcome).toBe("complete");
  expect(saved.result.multiServer.failures).toEqual([]);
  const preflight = await fetch(`${home.http}/preflight`);
  const identity = await preflight.json();
  expect(saved.result.multiServer.selection[0].name).toBe(identity.server.name);
  expect(saved.engine).toBe(identity.engineVersion);
  for (const stage of [
    "latency",
    "download",
    "upload",
    "bidirectional",
  ] as const)
    expect(saved.result.stages[stage]).toBe("complete");
  expect(saved.result.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
  expect(saved.result.download?.reportedBytesPerSec).toBeGreaterThan(0);
  expect(saved.result.multiServer.servers[0].throughput.transport).toBe(
    "fetch-stream",
  );
  expect(saved.result.multiServer.servers[0].latencyTarget?.transport).toBe(
    "websocket",
  );
  expect(saved.result.multiServer.servers[0].throughput?.origin).toBe(
    home.http,
  );

  const readouts = (root: string) =>
    page.evaluate(
      (root) =>
        [...document.querySelectorAll(`${root} .card .headline`)].map((card) =>
          card.textContent!.replace(/\s+/g, " ").trim(),
        ),
      root,
    );
  const live = await readouts(".results");
  expect(live.length).toBeGreaterThan(0);
  const transferred = await page
    .locator("footer.status .transferred .readout")
    .textContent();
  await page.goto(`${home.http}/#/history/${saved.id}`);
  await expect(page.locator(".detail-pane .card")).toHaveCount(live.length);
  expect(await readouts(".detail-pane")).toEqual(live);
  await expect(page.locator(".detail-pane .head-facts")).toContainText(
    new RegExp(`Transferred\\s*${transferred}`),
  );
  await page.goto(`${home.http}/#/`);

  const row = page.locator(`a.result-row[data-history-id="${saved.id}"]`);
  await page.getByRole("button", { name: "History", exact: true }).click();
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
  // A running clock would redraw its 0.1 s readout within eight frames.
  await page.evaluate(async () => {
    for (let frame = 0; frame < 8; frame++)
      await new Promise((resolve) => requestAnimationFrame(resolve));
  });
  expect(await elapsed.textContent()).toBe(frozen);
  const saved = await run(page);
  expect(saved.result.outcome).toBe("complete");
});

test("reduced motion still updates every readout and completes", async (page) => {
  await page.cdp("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-reduced-motion", value: "reduce" }],
  });
  await open(page);
  await ready(page);
  await runButton(page, "Start test").click();
  await expect(phase(page, "download")).toHaveCount(1, { timeout: 10_000 });
  const elapsed = page.locator(".elapsed .readout");
  const first = await elapsed.textContent();
  await expect
    .poll(async () => (await elapsed.textContent()) !== first)
    .toBe(true);
  await expect(page.locator(".gauge-value")).toHaveText(/\d/);
  await expect(
    page.locator('.results .card[data-tone="download"] .graph'),
  ).toBeVisible();
  const saved = await savedResult(page);
  expect(saved.result.outcome).toBe("complete");
  await expect(
    page.locator('.results .card[data-tone="download"] .facts'),
  ).toContainText("Peak");
});

test("a run in a hidden tab completes and saves", async (page) => {
  await open(page);
  await ready(page);
  // Closing Settings leaves its toggle's wash fading on the compositor; Chrome freezes a page minimized mid-fade.
  await page.evaluate(settled);
  const { windowId } = await page.cdp("Browser.getWindowForTarget");
  const bounds = (windowState: string) =>
    page.cdp("Browser.setWindowBounds", { windowId, bounds: { windowState } });
  const startedAt = Date.now();
  await runButton(page, "Start test").click();
  await bounds("minimized");
  expect(await page.evaluate(() => document.visibilityState)).toBe("hidden");
  const saved = await savedResult(page, startedAt, 20_000);
  await bounds("normal");
  expect(saved.result.outcome).toBe("complete");
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
  expect(await page.locator(".phase-toast.visible").state()).toEqual([]);
  for (const [width, height] of viewports)
    for (const scheme of ["light", "dark"]) {
      await page.setViewportSize({ width, height });
      // Contrast is judged on settled values; a fade in flight would lower it.
      await page.cdp("Emulation.setEmulatedMedia", {
        features: [
          { name: "prefers-color-scheme", value: scheme },
          { name: "prefers-reduced-motion", value: "reduce" },
        ],
      });
      await openSettings(page);
      const layout = () =>
        page.evaluate(() => {
          const overflow = [
            ...document.querySelectorAll(".panel-body, .stage"),
          ].filter((el) => el.scrollWidth > el.clientWidth + 1);
          const stage = document
            .querySelector(".gauge-panel .dial")!
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
  const identity = await (await fetch(`${home.http}/preflight`)).json();
  const legal: LegalAbout = await (
    await fetch(`${home.http}/legal/about.json`)
  ).json();
  const rust = identity.engineVersion.endsWith("-rust");
  expect(
    legal.components.some(
      (component) => component.ecosystem === (rust ? "cargo" : "go"),
    ),
  ).toBe(true);
  if (rust) {
    expect(legal.sourceVersion).toBe(identity.engineVersion);
    expect(
      legal.components.some((component) => component.ecosystem === "go"),
    ).toBe(false);
  }
  await expect(dialog).toContainText(rust ? "Rust crates" : "Go modules");
  const links = legal.components.flatMap((component) => {
    expect(component.links.length).toBeGreaterThan(0);
    return component.links.map((link) => {
      expect(new URL(link.url).protocol).toMatch(/^https?:$/);
      return { label: `${link.label} for ${component.name}`, href: link.url };
    });
  });
  expect(
    await dialog.evaluate((el: HTMLElement) =>
      Array.from(el.querySelectorAll(".component-links a"), (link) => ({
        label: link.getAttribute("aria-label"),
        href: link.getAttribute("href"),
      })),
    ),
  ).toEqual(links);
  const svelte = legal.components.find(
    (component) => component.name === "svelte",
  )!;
  expect(svelte.modified).toBe(true);
  expect(
    decodeURIComponent(
      svelte.links.find((link) => link.label === "Changes")!.url,
    ),
  ).toContain(`/client/patches/svelte@${svelte.version}.patch`);
  if (rust) {
    const fork = legal.components.find(
      (component) => component.name === "noq",
    )!;
    expect(fork.links.map((link) => link.label)).toEqual([
      "Source",
      "Upstream",
      "Changes",
    ]);
    expect(fork.links[0].url).toMatch(
      /^https:\/\/github\.com\/zR-JB\/noq\/tree\/[0-9a-f]{40}$/,
    );
    expect(fork.links[1].url).toMatch(
      /^https:\/\/github\.com\/n0-computer\/noq\/tree\/[0-9a-f]{40}$/,
    );
  }
  expect(await seriousViolations(page, '[role="dialog"]')).toEqual([]);
  await page.raw.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(about).toBeFocused();
});
