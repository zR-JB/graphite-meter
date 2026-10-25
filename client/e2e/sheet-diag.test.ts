import { open } from "./fleet";
import { browser, expect, test } from "./webview";

for (const rate of [1, 4, 8])
  for (const variant of ["old", "new"] as const)
    test(`sheet ${variant} at cpu ${rate}`, async (page) => {
      if (browser === "chrome") await page.cdp("Emulation.setCPUThrottlingRate", { rate });
      else if (rate > 1) return;
      if (variant === "old") {
        await page.setViewportSize({ width: 1600, height: 900 });
        await open(page);
        const travel = await page.evaluate(async () => {
          const key = (key: string) =>
            document.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
          const wait = (ms: number) => new Promise((done) => setTimeout(done, ms));
          const sheet = () =>
            document.querySelector<HTMLElement>('dialog[aria-label="Details"]:not([aria-hidden])')!;
          key("d");
          await wait(600);
          key("d");
          await wait(60);
          // Mid-way out it is still on screen: the sheet itself, or a copy where a browser cannot keep it displayed.
          const leaving = [...document.querySelectorAll('dialog[aria-label="Details"]')].some(
            (el) => el.checkVisibility() && el.getBoundingClientRect().left < innerWidth,
          );
          key("d");
          const lefts: number[] = [];
          const end = performance.now() + 500;
          while (performance.now() < end) {
            await new Promise((done) => requestAnimationFrame(done));
            lefts.push(sheet().getBoundingClientRect().left);
          }
          return { leaving, lefts, width: innerWidth };
        });
        expect(travel.leaving).toBe(true);
        expect(Math.min(...travel.lefts)).toBeGreaterThan(travel.width / 2);
      } else {
        await page.setViewportSize({ width: 1600, height: 900 });
        await open(page);
        const travel = await page.evaluate(async () => {
          const key = (key: string) =>
            document.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
          const frame = () => new Promise((done) => requestAnimationFrame(done));
          const sheet = () =>
            document.querySelector<HTMLElement>('dialog[aria-label="Details"]:not([aria-hidden])')!;
          const lefts = () =>
            [...document.querySelectorAll('dialog[aria-label="Details"]')]
              .filter((el) => el.checkVisibility())
              .map((el) => el.getBoundingClientRect().left);
          key("d");
          let docked = NaN;
          for (let n = 0, was = NaN; n < 300 && !(docked < innerWidth && docked === was); n++) {
            await frame();
            [was, docked] = [docked, lefts()[0] ?? NaN];
          }
          key("d");
          // Mid-way out it is still on screen: the sheet itself, or a copy where a browser cannot keep it displayed.
          let leaving = false;
          for (let n = 0; n < 60 && !leaving; n++) {
            await frame();
            leaving = lefts().some((left) => left > docked && left < innerWidth);
          }
          key("d");
          const back: number[] = [];
          const end = performance.now() + 500;
          while (performance.now() < end) {
            await frame();
            back.push(sheet().getBoundingClientRect().left);
          }
          return { leaving, back, width: innerWidth };
        });
        expect(travel.leaving).toBe(true);
        expect(Math.min(...travel.back)).toBeGreaterThan(travel.width / 2);
      }
      console.log("DIAG sheet", variant, rate, "ok");
    });
