import {
  expect,
  openApp,
  prepareApp,
  startTest,
  test,
  waitForCompletion,
} from "./webview";

for (const viewport of [
  { width: 1280, height: 720 },
  { width: 1440, height: 900 },
  { width: 1024, height: 768 },
]) {
  test(`three-stage results stay visible with centered controls at ${viewport.width}×${viewport.height}`, async ({
    page,
  }) => {
    await openApp(page, "dummy", viewport);
    const settings = await prepareApp(page, "three-stage", "dummy", viewport);
    await settings.getByRole("button", { name: "Close Settings" }).click();
    await startTest(page);
    await waitForCompletion(page, 10000);
    const geometry = await page.evaluate(() => {
      const element = (selector: string) => {
        const node = document.querySelector(selector);
        if (!(node instanceof HTMLElement))
          throw new Error(`missing ${selector}`);
        return node;
      };
      const gauge = element(".gauge-panel .stage").getBoundingClientRect();
      const profile = element(".latency-panel");
      const profileBox = profile.getBoundingClientRect();
      const stage = element("#console > section.stage");
      const button = element(".run-button").getBoundingClientRect();
      const buttonLabel = element(
        ".run-button-content",
      ).getBoundingClientRect();
      const durationTag = element(
        ".run-button .duration",
      ).getBoundingClientRect();
      const rail = element(".stage-head").getBoundingClientRect();
      const host = element(".gauge-panel").getBoundingClientRect();
      return {
        stageOverflow: stage.scrollHeight - stage.clientHeight,
        profileOverflow: profile.scrollHeight - profile.clientHeight,
        profileScroll: getComputedStyle(profile).overflowY,
        gaugeHeight: gauge.height,
        pairedHeight: Math.abs(gauge.height - profileBox.height),
        pairedWidth: Math.abs(gauge.width - profileBox.width),
        buttonLabelCenter: Math.abs(
          buttonLabel.left +
            buttonLabel.width / 2 -
            button.left -
            button.width / 2,
        ),
        durationSeparate:
          durationTag.left >= buttonLabel.right + 8 &&
          durationTag.right < button.right,
        buttonCenter: Math.abs(
          button.left + button.width / 2 - host.left - host.width / 2,
        ),
        railCenter: Math.abs(
          rail.left + rail.width / 2 - host.left - host.width / 2,
        ),
        controlsSeparated: rail.top >= button.bottom,
        lanesInside: [...profile.querySelectorAll(".lane")].every(
          (lane) => lane.getBoundingClientRect().bottom <= profileBox.bottom,
        ),
        sharedAxis: profile.querySelectorAll(".ticks").length,
        wireAligned: [...document.querySelectorAll(".result-card")].every(
          (card) => {
            const estimate = card.querySelector(".est-num");
            const rate = card.querySelector(".val .num");
            return (
              !estimate ||
              !rate ||
              Math.abs(
                estimate.getBoundingClientRect().left -
                  rate.getBoundingClientRect().left,
              ) <= 1
            );
          },
        ),
      };
    });
    expect(geometry.stageOverflow).toBeLessThanOrEqual(1);
    expect(geometry.profileOverflow).toBeLessThanOrEqual(1);
    expect(geometry.profileScroll).toBe("visible");
    expect(geometry.gaugeHeight).toBeGreaterThanOrEqual(280);
    expect(geometry.pairedHeight).toBeLessThanOrEqual(1);
    expect(geometry.pairedWidth).toBeLessThanOrEqual(1);
    expect(geometry.buttonCenter).toBeLessThanOrEqual(1);
    expect(geometry.buttonLabelCenter).toBeLessThanOrEqual(1);
    expect(geometry.durationSeparate).toBe(true);
    expect(geometry.railCenter).toBeLessThanOrEqual(1);
    expect(geometry.controlsSeparated).toBe(true);
    expect(geometry.lanesInside).toBe(true);
    expect(geometry.sharedAxis).toBe(1);
    expect(geometry.wireAligned).toBe(true);
  });
}

for (const width of [320, 390]) {
  test(`phone ${width}px keeps a prominent dial and a natural run-to-stages reading order`, async ({
    page,
  }) => {
    await openApp(page, "dummy", { width, height: 844 });
    const settings = await prepareApp(page, "three-stage", "dummy", {
      width,
      height: 844,
    });
    await settings.getByRole("button", { name: "Close Settings" }).click();
    const boxes = await page.evaluate(() =>
      [
        ".gauge-panel .stage",
        ".run-button",
        ".stage-head",
        ".latency-panel",
      ].map((selector) => {
        const node = document.querySelector(selector);
        if (!node) throw new Error(`missing ${selector}`);
        const box = node.getBoundingClientRect();
        return { top: box.top, bottom: box.bottom, height: box.height };
      }),
    );
    expect(boxes[0].height).toBeGreaterThanOrEqual(280);
    for (let i = 1; i < boxes.length; i++)
      expect(boxes[i].top).toBeGreaterThanOrEqual(boxes[i - 1].bottom);
    await expect(page.locator(".run-button")).toHaveAttribute(
      "aria-describedby",
      "run-duration",
    );
    await startTest(page);
    await waitForCompletion(page, 10000);
    const labelsFit = await page.evaluate(() =>
      [
        ...document.querySelectorAll<HTMLElement>(".seg-label, .lane-label"),
      ].every((label) => label.scrollWidth <= label.clientWidth),
    );
    expect(labelsFit).toBe(true);
  });
}
