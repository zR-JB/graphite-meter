import {
  baseConfig,
  closeSettings,
  home,
  open,
  openSettings,
  phase,
  ready,
  run,
  runButton,
  savedResult,
} from "./fleet";
import { expect, settled, test } from "./webview";

// Flush native dialog/popover events without waiting for CSS transitions: each
// reversal should interrupt the preceding animation, just like rapid input.
async function churnSurfaces(cycles: number) {
  const frames = async () => {
    for (let i = 0; i < 2; i++)
      await new Promise<void>((done) => requestAnimationFrame(() => done()));
  };
  const click = async (selector: string) => {
    const button = document.querySelector<HTMLButtonElement>(selector);
    // A close that steps back through history lands in a later task; a control under the closing modal waits.
    for (let wait = 0; button?.closest("[inert]") && wait < 30; wait++)
      await frames();
    if (!button || button.disabled || !button.checkVisibility())
      throw new Error(`Unavailable stress control: ${selector}`);
    button.click();
    await frames();
  };
  const topbar = async (label: string, overflow: string) => {
    const selector = `.topbar button[aria-label="${label}"]`;
    if (document.querySelector(selector)?.checkVisibility())
      await click(selector);
    else {
      await click('.topbar button[aria-label="More controls"]');
      await click(`.topbar [data-more="${overflow}"]`);
    }
  };
  const assertClosed = async () => {
    const open = () =>
      document.querySelector(
        "dialog[open], :popover-open:not(.tooltip), .scrim.open",
      );
    for (let wait = 0; open() && wait < 60; wait++) await frames();
    if (open()) throw new Error(`Surface stuck open at ${location.hash}`);
  };
  for (let i = 0; i < cycles; i++) {
    for (let reversal = 0; reversal < 2; reversal++) {
      await topbar("Settings", "");
      const reset = 'dialog[aria-label="Settings"] button.reset';
      // Reset is deliberately unavailable while a measurement is running.
      if (!document.querySelector<HTMLButtonElement>(reset)?.disabled) {
        await click(reset);
        const confirm = document.querySelector<HTMLDialogElement>(
          'dialog[role="alertdialog"][open]',
        );
        if (!confirm?.matches(":modal"))
          throw new Error("Settings confirmation did not open modally");
        await click('dialog[role="alertdialog"][open] .confirm-actions button');
      }
      await click('button[aria-label="Close Settings"]');
      await assertClosed();
    }
    for (let reversal = 0; reversal < 2; reversal++) {
      await topbar("Details", "endpoint");
      await click('dialog[aria-label="Details"] button.link-row:not(.copy)');
      if (!document.querySelector(".legal-dialog:modal"))
        throw new Error("Legal dialog did not open modally");
      await click('button[aria-label="Close About & legal"]');
      await click('button[aria-label="Close Details"]');
      await assertClosed();
    }
    await topbar("History", "history");
    // Its first visit loads a chunk; later visits deliberately do not settle.
    for (
      let wait = 0;
      !document.querySelector('button[aria-label="History actions"]');
      wait++
    ) {
      if (wait === 60) throw new Error("History did not open");
      await frames();
    }
    const columnsTrigger = 'button[aria-label="Choose columns and sort order"]';
    if (document.querySelector(columnsTrigger)?.checkVisibility()) {
      await click(columnsTrigger);
      const columns = document.querySelector<HTMLElement>(
        '[aria-label="History view options"]',
      );
      if (!columns?.matches(":popover-open"))
        throw new Error("History view options did not open");
      // A second activation exercises native popover toggle coalescing.
      await click(columnsTrigger);
    }
    for (let reversal = 0; reversal < 2; reversal++) {
      await click('button[aria-label="History actions"]');
      await click('button[aria-label="History actions"]');
    }
    await click('button[aria-label="Close History"]');
    await assertClosed();
  }
}

for (const width of [1600, 1000, 390]) {
  test.chrome(
    `rapid surface reversals release resources at ${width}px, including during a run`,
    async (page) => {
      await page.setViewportSize({ width, height: 900 });
      await page.addInitScript(() => {
        const pending = new Set<number>();
        const raf = window.requestAnimationFrame.bind(window);
        const cancel = window.cancelAnimationFrame.bind(window);
        window.requestAnimationFrame = (fn) => {
          const id = raf((now) => {
            pending.delete(id);
            fn(now);
          });
          pending.add(id);
          return id;
        };
        window.cancelAnimationFrame = (id) => {
          pending.delete(id);
          cancel(id);
        };
        Object.assign(window, { surfaceFrames: pending });
      });
      await open(page, home.http, {
        config: {
          stages: { ...baseConfig.stages, latency: false, upload: false },
          duration: { ...baseConfig.duration, downloadMs: 5000 },
        },
      });
      await ready(page);
      await run(page);
      await page.cdp("Emulation.setCPUThrottlingRate", { rate: 4 });
      await page.evaluate(churnSurfaces, 1);
      await page.evaluate(settled);
      await page.cdp("HeapProfiler.collectGarbage");
      const before = await page.cdp("Memory.getDOMCounters");
      const started = Date.now();
      await runButton(page, "Run again").click();
      await expect(phase(page, "download")).toHaveCount(1);
      // Each cycle must answer within the harness deadline; the complete batch
      // can exceed it on a CPU-throttled CI runner without a stalled page.
      for (let cycle = 0; cycle < 10; cycle++)
        await page.evaluate(churnSurfaces, 1);
      const saved = await savedResult(page, started, 30_000);
      expect(saved.result.outcome).toBe("complete");
      for (let cycle = 0; cycle < 10; cycle++)
        await page.evaluate(churnSurfaces, 1);
      await page.evaluate(settled);
      await page.cdp("HeapProfiler.collectGarbage");
      const after = await page.cdp("Memory.getDOMCounters");
      expect(after.nodes - before.nodes).toBeLessThan(100);
      expect(after.jsEventListeners - before.jsEventListeners).toBeLessThan(10);
      expect(
        await page.evaluate(() => (window as any).surfaceFrames.size),
      ).toBe(0);
      expect(
        await page.evaluate(
          () =>
            document.querySelectorAll(
              "dialog[open], :popover-open:not(.tooltip)",
            ).length,
        ),
      ).toBe(0);
      expect(
        await page.evaluate(
          () =>
            !!document.activeElement?.isConnected &&
            !document.activeElement.closest("[inert]"),
        ),
      ).toBe(true);
    },
    // Twenty complete surface cycles under CPU throttling can exceed the
    // ordinary test timeout when CI runs other browser files concurrently.
    { monitorDisplay: false, timeout: 180_000 },
  );
}

for (const width of [1000, 390]) {
  test(`a closing panel releases pointer input at ${width}px before its fade finishes`, async (page) => {
    await page.setViewportSize({ width, height: 900 });
    await open(page, home.http);
    await ready(page);
    const settings = await openSettings(page);
    await settings.getByRole("button", { name: "Close Settings" }).click();
    await expect
      .poll(() =>
        settings.all((nodes: HTMLElement[]) =>
          nodes.every((node) => node.inert),
        ),
      )
      .toBe(true);
    const button = runButton(page, "Start test");
    const target = await button.evaluate((node: HTMLElement) => {
      const box = node.getBoundingClientRect();
      const x = box.x + box.width / 2;
      const y = box.y + box.height / 2;
      const hit = document.elementFromPoint(x, y);
      return { x, y, receivesInput: !!hit && node.contains(hit) };
    });
    expect(target.receivesInput).toBe(true);
    // Bypass the locator's actionability retry: the first click must land while
    // the closed sheet and scrim are still fading.
    await page.click(target.x, target.y);
    await expect(runButton(page, "Stop test")).toBeVisible();
    await runButton(page, "Stop test").click();
    await expect(phase(page, "aborted")).toHaveCount(1);
  });
}

test("panels build as the pointer reaches their key and retain their state after closing", async (page) => {
  await open(page, home.http);
  await expect(page.locator("#console")).toHaveCount(1);
  const duration = page.locator('input[aria-label="Download stage time"]');
  await expect(duration).toHaveCount(0);
  await expect(page.locator(".infra")).toHaveCount(0);
  await page.getByRole("button", { name: "Settings", exact: true }).hover();
  await expect(duration).toHaveCount(1);
  expect(
    await duration.all((els) => els.some((el) => el.checkVisibility())),
  ).toBe(false);
  await page.getByRole("button", { name: "Details", exact: true }).hover();
  await expect(page.locator(".infra")).toHaveCount(1);
  await openSettings(page);
  await duration.fill("2");
  await closeSettings(page);
  await expect(duration).toHaveCount(1);
  await openSettings(page);
  await expect(duration).toHaveAttribute("aria-valuenow", "2");
  await closeSettings(page);
  await page.getByRole("button", { name: "Details", exact: true }).click();
  await expect(page.locator(".infra")).toHaveCount(1);
  await page
    .getByRole("button", { name: "Close Details", exact: true })
    .click();
  await expect(page.locator(".infra")).toHaveCount(1);
});

test.chrome(
  "rapid settings disclosures finish without duplicated rows or retained listeners",
  async (page) => {
    await open(page, home.http);
    await ready(page);
    await openSettings(page);
    const cycle = async (repetitions: number) => {
      const frame = () =>
        new Promise<void>((done) => requestAnimationFrame(() => done()));
      const presets = document.querySelector('[aria-label="Duration preset"]')!;
      const medium = Array.from(presets.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Medium",
      )!;
      const custom = Array.from(presets.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Custom",
      )!;
      for (let i = 0; i < repetitions; i++) {
        custom.click();
        await frame();
        medium.click();
        await frame();
        for (const fold of document.querySelectorAll<HTMLButtonElement>(
          ".picker button.fold",
        )) {
          fold.click();
          await frame();
          fold.click();
          await frame();
        }
      }
    };
    await page.evaluate(cycle, 1);
    await page.evaluate(settled);
    await page.cdp("HeapProfiler.collectGarbage");
    const before = await page.cdp("Memory.getDOMCounters");
    await page.cdp("Emulation.setCPUThrottlingRate", { rate: 4 });
    await page.evaluate(cycle, 30);
    await page.evaluate(settled);
    await page.cdp("HeapProfiler.collectGarbage");
    const after = await page.cdp("Memory.getDOMCounters");
    expect(after.nodes - before.nodes).toBeLessThan(50);
    expect(after.jsEventListeners - before.jsEventListeners).toBeLessThan(5);
    expect(
      await page.evaluate(() => {
        const presets = document.querySelector(
          '[aria-label="Duration preset"]',
        )!;
        return presets.closest("section")!.querySelectorAll(".stage-row")
          .length;
      }),
    ).toBe(1);
    expect(
      await page.evaluate(() =>
        Array.from(document.querySelectorAll(".picker button.fold"), (button) =>
          button.getAttribute("aria-expanded"),
        ),
      ),
    ).not.toContain("true");
  },
);

test("reopening legal notices retains the loaded rows", async (page) => {
  await open(page, home.http);
  await page.getByRole("button", { name: "Details", exact: true }).click();
  await page.getByRole("button", { name: "About & legal" }).click();
  const legal = page.getByRole("dialog", { name: "About & legal" });
  await expect(legal).toContainText("Third-party software");
  await page.evaluate(() => {
    const thirdParty = document.querySelector(".legal-dialog .third-party")!;
    const removed: Node[] = [];
    const observer = new MutationObserver((mutations) => {
      for (const mutation of mutations) removed.push(...mutation.removedNodes);
    });
    observer.observe(thirdParty.parentNode!, {
      childList: true,
      subtree: true,
    });
    Object.assign(window, { legalRows: { thirdParty, removed, observer } });
  });
  await page.getByRole("button", { name: "Close About & legal" }).click();
  await page.getByRole("button", { name: "About & legal" }).click();
  await expect(legal).toContainText("Third-party software");
  const retained = await page.evaluate(() => {
    const rows = (window as any).legalRows;
    rows.observer.disconnect();
    return {
      same:
        rows.thirdParty ===
        document.querySelector(".legal-dialog .third-party"),
      removed: rows.removed.length,
    };
  });
  expect(retained).toEqual({ same: true, removed: 0 });
});

test.chrome(
  "a live stage keeps its open tooltip through progress and phase changes",
  async (page) => {
    await open(page, home.http, {
      config: { duration: { ...baseConfig.duration, downloadMs: 2_500 } },
    });
    await ready(page);
    const started = Date.now();
    await runButton(page, "Start test").click();
    await expect(phase(page, "download")).toHaveCount(1);
    const chip = page.locator(
      '.stage-track [data-tone="download"][role="switch"]',
    );
    const point = await chip.evaluate((node: Element) => {
      const box = node.getBoundingClientRect();
      return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
    });
    await page.cdp("Input.dispatchMouseEvent", {
      type: "mouseMoved",
      ...point,
    });
    const tip = page.locator('[role="tooltip"]');
    await expect(tip).toHaveCount(1);
    const id = await tip.getAttribute("id");
    await Bun.sleep(500);
    expect(await tip.getAttribute("id")).toBe(id);
    await savedResult(page, started, 20_000);
    await page.cdp("Input.dispatchMouseEvent", {
      type: "mouseMoved",
      x: 0,
      y: 0,
    });
    await expect(tip).toHaveCount(0);
  },
);

test.chrome(
  "repeated runs release their views and keep tooltip anchor styles bounded",
  async (page) => {
    await open(page, home.http, {
      config: {
        stages: {
          latency: true,
          download: true,
          upload: true,
          bidirectional: true,
        },
        transports: {
          throughputTarget: "protocol:http1",
          latencyTarget: "transport:websocket",
        },
      },
    });
    await ready(page);
    const counters = [];
    for (let i = 0; i < 3; i++) {
      await run(page);
      const anchors = await page.evaluate(() =>
        Array.from(
          document.querySelectorAll<HTMLElement>("[style]"),
          (node) =>
            node.style
              .getPropertyValue("anchor-name")
              .split(",")
              .filter((name) => name.trim().startsWith("--gm-tt-")).length,
        ),
      );
      expect(Math.max(0, ...anchors)).toBeLessThanOrEqual(1);
      await page.goto(`${home.http}/#/history`);
      await expect(page.locator(".result-row")).toHaveCount(i + 1);
      await page.goto(`${home.http}/#/`);
      await page.evaluate(settled);
      await page.cdp("HeapProfiler.collectGarbage");
      counters.push(await page.cdp("Memory.getDOMCounters"));
    }
    expect(counters[2].nodes - counters[0].nodes).toBeLessThan(100);
    expect(
      counters[2].jsEventListeners - counters[0].jsEventListeners,
    ).toBeLessThan(10);
  },
);
