import {
  expect,
  openApp,
  openSettings,
  openEndpointInfo,
  prepareApp,
  startTest,
  test,
  waitForCompletion,
} from "./webview";

test("three-stage results remain usable across desktop and phone layouts", async ({
  page,
}) => {
  const settings = await prepareApp(page, "three-stage", "dummy", {
    width: 1280,
    height: 720,
  });
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startTest(page);
  await waitForCompletion(page, 10000);
  for (const viewport of [
    { width: 1280, height: 720 },
    { width: 1440, height: 900 },
    { width: 2048, height: 1050 },
    { width: 2048, height: 1152 },
    { width: 2560, height: 1440 },
    { width: 1024, height: 768 },
  ]) {
    await page.setViewportSize(viewport);
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
      const controls = element(".instrument-controls").getBoundingClientRect();
      const results = element(".results-slot").getBoundingClientRect();
      const chart = element(".chart").getBoundingClientRect();
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
        controlsCenter: Math.abs(
          controls.left + controls.width / 2 - host.left - host.width / 2,
        ),
        controlsAligned: Math.abs(rail.bottom - button.bottom) <= 1,
        controlsSeparated: rail.left >= button.right + 16,
        controlsStacked: rail.top >= button.bottom + 12,
        instrumentWidth: host.width,
        chartHeight: chart.height,
        stackedReadouts: [...document.querySelectorAll(".result-card")].every(
          (card) => {
            const header = card
              .querySelector("header")!
              .getBoundingClientRect();
            const value = card.querySelector(".val")!.getBoundingClientRect();
            return (
              value.top >= header.bottom + 4 &&
              Math.abs(value.left - header.left) <= 1
            );
          },
        ),
        resultWidths: [...document.querySelectorAll(".result-card")].map(
          (card) => card.getBoundingClientRect().width,
        ),
        groupSpacing:
          button.top - gauge.bottom >= 16 &&
          results.top - button.bottom >= 16 &&
          chart.top - results.bottom >= 8,
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
    expect(geometry.controlsCenter).toBeLessThanOrEqual(1);
    expect(geometry.buttonLabelCenter).toBeLessThanOrEqual(1);
    expect(geometry.durationSeparate).toBe(true);
    if (viewport.height <= 800) {
      expect(geometry.controlsAligned).toBe(true);
      expect(geometry.controlsSeparated).toBe(true);
    } else expect(geometry.controlsStacked).toBe(true);
    expect(geometry.instrumentWidth).toBeLessThanOrEqual(1920);
    if (viewport.width === 2048)
      expect(geometry.instrumentWidth).toBeGreaterThanOrEqual(1800);
    expect(geometry.chartHeight).toBeLessThanOrEqual(360);
    expect(geometry.resultWidths.every((width) => width <= 280)).toBe(true);
    expect(geometry.stackedReadouts).toBe(true);
    expect(geometry.groupSpacing).toBe(true);
    expect(geometry.lanesInside).toBe(true);
    expect(geometry.sharedAxis).toBe(3);
    expect(geometry.wireAligned).toBe(true);
    await page.artifact(`instrument-${viewport.width}x${viewport.height}`);
    if (viewport.width === 2048 && viewport.height === 1152) {
      const settings = await openSettings(page);
      await page.artifact("instrument-settings-2048");
      const info = await openEndpointInfo(page);
      const fit = await page.evaluate(() => {
        const stage =
          document.querySelector<HTMLElement>(".measurement-stage")!;
        const gauge = document
          .querySelector(".gauge-panel")!
          .getBoundingClientRect();
        const chart = document.querySelector(".chart")!.getBoundingClientRect();
        return {
          width: gauge.width,
          aligned:
            Math.abs(gauge.left - chart.left) < 1 &&
            Math.abs(gauge.width - chart.width) < 1,
          overflow: stage.scrollWidth - stage.clientWidth,
        };
      });
      expect(fit.width).toBeGreaterThanOrEqual(760);
      expect(fit.aligned).toBe(true);
      expect(fit.overflow).toBeLessThanOrEqual(1);
      await page.artifact("instrument-both-docks-2048");
      await settings.getByRole("button", { name: "Close Settings" }).click();
      await page.artifact("instrument-info-2048");
      await info.getByRole("button", { name: "Close Endpoint" }).click();
    }
  }
  for (const width of [320, 390]) {
    await page.setViewportSize({ width, height: width === 320 ? 740 : 844 });
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
    const labelsFit = await page.evaluate(() =>
      [
        ...document.querySelectorAll<HTMLElement>(".seg-label, .lane-label"),
      ].every((label) => label.scrollWidth <= label.clientWidth),
    );
    expect(labelsFit).toBe(true);
    await page.artifact(`instrument-${width}`);
  }
});

test("single and four-lane profiles keep bounded plots and clear gauge notes", async ({
  page,
}) => {
  await openApp(page, "dummy", { width: 2560, height: 1440 });
  for (const allStages of [false, true]) {
    await page.evaluate(
      (all) =>
        localStorage.setItem(
          "graphite-meter:v1",
          JSON.stringify({
            config: {
              stages: {
                latency: true,
                download: all,
                upload: all,
                bidirectional: all,
              },
            },
          }),
        ),
      allStages,
    );
    await page.reload();
    for (const viewport of [
      { width: 2560, height: 1440 },
      { width: 1440, height: 640 },
      { width: 320, height: 740 },
    ]) {
      await page.setViewportSize(viewport);
      const geometry = await page.evaluate(() => {
        const box = (selector: string) =>
          document.querySelector(selector)!.getBoundingClientRect();
        const tracks = [...document.querySelectorAll(".live-profile .track")];
        return {
          laneCount: tracks.length,
          plotHeights: tracks.map(
            (track) => track.getBoundingClientRect().height,
          ),
          noteGap: box(".gauge-notes").top - box(".gauge-face").bottom,
          contained: document.documentElement.scrollWidth <= innerWidth,
        };
      });
      expect(geometry.laneCount).toBe(allStages ? 4 : 1);
      expect(
        geometry.plotHeights.every((height) => height >= 32 && height <= 42),
      ).toBe(true);
      expect(geometry.noteGap).toBeGreaterThanOrEqual(8);
      expect(geometry.contained).toBe(true);
      await page.artifact(
        `instrument-${allStages ? "four" : "single"}-${viewport.width}x${viewport.height}`,
      );
    }
  }
});
