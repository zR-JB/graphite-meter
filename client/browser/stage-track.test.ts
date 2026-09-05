import {
  abortButton,
  againButton,
  expect,
  expectNear,
  expectVisible,
  openApp,
  openSettings,
  prepareApp,
  startTest,
  test,
} from "./webview";
function parseElapsed(text: string): number {
  const match = text.match(/elapsed\s+([0-9]+(?:\.[0-9]+)?)s/);
  return match ? Number(match[1]) : 0;
}
type Geometry = Record<string, { top: number; height: number }>;
function expectSameGeometry(reference: Geometry, ...samples: Geometry[]) {
  for (const sample of samples)
    for (const selector of Object.keys(reference)) {
      expectNear(sample[selector].top, reference[selector].top);
      expectNear(sample[selector].height, reference[selector].height);
    }
}
test("terminal stage switches select the next run without erasing retained status", async ({
  page,
}) => {
  await prepareApp(page, "short");
  await startTest(page);
  await expectVisible(againButton(page), 5_000);
  const terminalCards = await page.evaluate(() => {
    const rail = document.querySelector(".stage-head");
    const cards = document.querySelector(".result-cards");
    if (!(rail instanceof HTMLElement) || !(cards instanceof HTMLElement))
      throw new Error("missing terminal result layout");
    return {
      railBottom: rail.getBoundingClientRect().bottom,
      cardsTop: cards.getBoundingClientRect().top,
      panelCenter:
        (document.querySelector(".gauge-panel")?.getBoundingClientRect().left ??
          0) +
        (document.querySelector(".gauge-panel")?.getBoundingClientRect()
          .width ?? 0) /
          2,
      cardsCenter:
        cards.getBoundingClientRect().left +
        cards.getBoundingClientRect().width / 2,
    };
  });
  expect(terminalCards.cardsTop).toBeGreaterThanOrEqual(
    terminalCards.railBottom - 1,
  );
  expect(
    Math.abs(terminalCards.cardsCenter - terminalCards.panelCenter),
  ).toBeLessThanOrEqual(1);
  const download = page.getByRole("switch", { name: "Download stage" });
  await expect(download).toHaveAttribute("aria-checked", "true");
  await expect(download).toHaveClass(/seg--complete/);
  const geometry = () =>
    page.evaluate(() =>
      Object.assign(
        Object.fromEntries(
          [".stage-head", ".gauge-panel .stage", ".run-slot", ".chart"].map(
            (selector) => {
              const stage = document.querySelector("#console > section.stage");
              stage?.scrollTo(0, 0);
              const box = document
                .querySelector(selector)
                ?.getBoundingClientRect();
              if (!box || !(stage instanceof HTMLElement))
                throw new Error(`missing ${selector}`);
              return [
                selector,
                {
                  top: box.top - stage.getBoundingClientRect().top,
                  height: box.height,
                },
              ];
            },
          ),
        ),
      ),
    );
  const beforeToggle = await geometry();
  await download.click();
  await expect(download).toHaveAttribute("aria-checked", "false");
  await expect(download).toHaveClass(/seg--disabled/);
  await expect(download).toContainText("skipped");
  const disabledGeometry = await geometry();
  await download.click();
  await expect(download).toHaveAttribute("aria-checked", "true");
  await expect(download).toHaveClass(/seg--complete/);
  const restoredGeometry = await geometry();
  expectSameGeometry(beforeToggle, disabledGeometry, restoredGeometry);
});
test("elapsed time freezes on abort and restarts on the next run", async ({
  page,
}) => {
  await prepareApp(page, "short");
  const status = page.locator("footer.status");
  await startTest(page);
  await expectVisible(againButton(page), 5_000);
  await expect
    .poll(async () => parseElapsed((await status.textContent()) ?? ""))
    .toBeGreaterThan(0);
  await againButton(page).click();
  const abort = abortButton(page);
  await expectVisible(abort, 5_000);
  await expect
    .poll(async () => parseElapsed((await status.textContent()) ?? ""), {
      timeout: 2_000,
    })
    .toBeGreaterThan(0.3);
  await abort.click();
  await expect(status).toContainText("Aborted");
  const abortedElapsed = parseElapsed((await status.textContent()) ?? "");
  expect(abortedElapsed).toBeGreaterThan(0);
  await page.waitForTimeout(400);
  expect(parseElapsed((await status.textContent()) ?? "")).toBe(abortedElapsed);
  await againButton(page).click();
  await expectVisible(abort, 5_000);
  expect(parseElapsed((await status.textContent()) ?? "")).toBeLessThan(
    abortedElapsed,
  );
  await expect
    .poll(async () => parseElapsed((await status.textContent()) ?? ""), {
      timeout: 2_000,
    })
    .toBeGreaterThan(0);
});
test("stage switches preserve the at-least-one measured-stage guard", async ({
  page,
}) => {
  await openApp(page);
  const latency = page.getByRole("switch", { name: "Latency stage" });
  const download = page.getByRole("switch", { name: "Download stage" });
  const upload = page.getByRole("switch", { name: "Upload stage" });
  await latency.click();
  await download.click();
  await expect(upload).toHaveAttribute("aria-checked", "true");
  await upload.click();
  await expect(upload).toHaveAttribute("aria-checked", "true");
});
test("bidirectional stays a Settings-only optional stage", async ({ page }) => {
  await openApp(page);
  const bidi = page.getByRole("switch", { name: /Bidirectional stage/ });
  await expect(bidi).toHaveCount(0);
  const settings = await openSettings(page);
  const include = settings.getByLabel("Include concurrent download + upload");
  await settings
    .locator("label.switch", {
      hasText: "Include concurrent download + upload",
    })
    .click();
  await expect(include).toBeChecked();
  await expectVisible(bidi);
  await bidi.click();
  await expect(bidi).toHaveCount(0);
  await expect(include).not.toBeChecked();
});
test("future-stage selection changes immediately during an active run", async ({
  page,
}) => {
  await prepareApp(page, "long-latency");
  await startTest(page);
  const download = page.getByRole("switch", { name: "Download stage" });
  await expect(download).toBeEnabled();
  await download.click();
  await expect(download).toHaveAttribute("aria-checked", "false");
  await expect(download).toHaveClass(/seg--disabled/);
  await download.click();
  await expect(download).toHaveAttribute("aria-checked", "true");
  await expect(download).toHaveClass(/seg--pending/);
  await page.getByRole("button", { name: "Abort test" }).click();
  await expectVisible(againButton(page));
  await expect(download).toHaveAttribute("aria-checked", "true");
});
test("warmup is owned by one stage while future stages remain toggleable", async ({
  page,
}) => {
  await prepareApp(page, "warmup");
  await startTest(page);
  await expect(page.locator("#console")).toHaveAttribute(
    "data-phase",
    "warmup",
  );
  await expect(page.locator(".seg-tag", { hasText: "running" })).toHaveCount(1);
  await expect(
    page.getByRole("switch", { name: "Download stage" }),
  ).toBeEnabled();
  await expect(
    page.getByRole("switch", { name: "Upload stage" }),
  ).toBeEnabled();
});

test("removing the final measurement during warmup leaves no active result", async ({
  page,
}) => {
  const settings = await prepareApp(page, {
    "Warmup ms": "1500",
    "Latency ms": "0",
    "Download ms": "0",
    "Upload ms": "900",
  });
  await startTest(page);
  await expect(page.locator("#console")).toHaveAttribute(
    "data-phase",
    "warmup",
  );
  await settings.getByLabel("Upload ms").fill("0");
  await expectVisible(againButton(page), 5_000);
  await expect(page.locator("#console")).toHaveAttribute(
    "data-phase",
    "complete",
  );
  await expect(page.locator(".result-card")).toHaveCount(0);
  await expect(page.locator(".seg--active, .seg--recovering")).toHaveCount(0);
  await expect(page.getByRole("switch", { name: "Upload stage" })).toHaveClass(
    /seg--pending/,
  );
});

test("three live result chips remain below the phase rail", async ({
  page,
}) => {
  await prepareApp(page, "live-chips");
  await startTest(page);
  await expect(page.locator(".result-chip")).toHaveCount(3, {
    timeout: 5_000,
  });
  const liveChips = await page.evaluate(() => {
    const rail = document.querySelector(".stage-head");
    const chips = document.querySelector(".result-chips");
    if (!(rail instanceof HTMLElement) || !(chips instanceof HTMLElement))
      throw new Error("missing live result layout");
    return {
      railBottom: rail.getBoundingClientRect().bottom,
      chipsTop: chips.getBoundingClientRect().top,
      panelCenter:
        (document.querySelector(".gauge-panel")?.getBoundingClientRect().left ??
          0) +
        (document.querySelector(".gauge-panel")?.getBoundingClientRect()
          .width ?? 0) /
          2,
      chipsCenter:
        chips.getBoundingClientRect().left +
        chips.getBoundingClientRect().width / 2,
    };
  });
  expect(liveChips.chipsTop).toBeGreaterThanOrEqual(liveChips.railBottom - 1);
  expect(
    Math.abs(liveChips.chipsCenter - liveChips.panelCenter),
  ).toBeLessThanOrEqual(1);
});
