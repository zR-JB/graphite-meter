// Section 2.1 without faults: WebTransport runs in one page after idle gaps.
import {
  attempt,
  budgetTest,
  link,
  measured,
  openApp,
  runConfig,
  shapedFleet,
  type Attempt,
} from "./harness";
import { browser, expect, test } from "../webview";

const pages: Attempt[][] = [];

test(
  "T0: four WebTransport runs in one page after gaps of 5, 16, 20 and 60 s measure every stage",
  async (page) => {
    const fleet = await shapedFleet([link(20, 200)], { signIn: true });
    try {
      await openApp(page, fleet, runConfig("transport:webtransport"));
      const runs: Attempt[] = [];
      pages.push(runs);
      for (const gap of [5, 16, 20, 60]) {
        await Bun.sleep(gap * 1000);
        runs.push(await attempt(page, fleet));
      }
      expect(runs.map(measured)).toEqual(runs.map(() => ({ outcome: "complete", failures: [] })));
    } finally {
      await fleet.close();
    }
  },
  {
    timeout: 300_000,
    failing:
      browser === "firefox"
        ? "until step 3: Firefox's own QUIC connections fill the server's per-address quota"
        : undefined,
  },
);

budgetTest("T0's runs", pages);
