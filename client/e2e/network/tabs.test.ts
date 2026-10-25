// Section 2.5: a second tab idle on the app, which in Firefox is a user context of its own.
import {
  attempt,
  link,
  measured,
  openApp,
  reached,
  runConfig,
  shapedFleet,
  start,
  type Fleet,
} from "./harness";
import { runButton } from "../fleet";
import { browser, expect, Page, test } from "../webview";

const wt = runConfig("transport:webtransport");

/** A second tab on the fleet's app; Chrome's tabs share the first's sign-in and settings. */
async function secondTab(fleet: Fleet) {
  const other = new Page();
  if (browser === "firefox") await openApp(other, fleet, wt);
  else await other.goto(fleet.servers[0]!.url);
  await expect(runButton(other, /^(Start test|Run again)$/)).toBeVisible();
  return other;
}

test(
  "T2: WebTransport runs complete with a second tab idle on the app",
  async (page) => {
    const fleet = await shapedFleet([link(20, 200)], { signIn: true });
    let other: Page | undefined;
    try {
      await openApp(page, fleet, wt);
      other = await secondTab(fleet);
      for (const gap of [5, 16]) {
        await Bun.sleep(gap * 1000);
        expect(measured(await attempt(page, fleet))).toEqual({ outcome: "complete", failures: [] });
      }
    } finally {
      await other?.close();
      await fleet.close();
    }
  },
  { timeout: 180_000 },
);

test(
  "T2: a second tab's Start waits while the first tab runs",
  async (page) => {
    const fleet = await shapedFleet([link(20, 200)], { signIn: true });
    let other: Page | undefined;
    try {
      await openApp(page, fleet, wt);
      other = await secondTab(fleet);
      const run = await start(page, fleet);
      await reached(page, "download");
      await runButton(other, /^(Start test|Run again)$/).click();
      await expect(other.locator("#console")).toContainText("A test is running in another tab");
      expect(await other.evaluate(() => !!document.querySelector(".run-button.running"))).toBe(
        false,
      );
      await run.end();
    } finally {
      await other?.close();
      await fleet.close();
    }
  },
  {
    timeout: 120_000,
    failing: {
      until: "until step 13: tabs share no lock on runs",
      error: /^contains: \[\{"kind":"css","value":"#console"\}\]/,
    },
  },
);
