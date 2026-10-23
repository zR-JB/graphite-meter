// Section 2.1 with faults: a UDP outage in one run and another client behind the browser's address.
import {
  budgetTest,
  link,
  measured,
  openApp,
  runConfig,
  shapedFleet,
  start,
  type Attempt,
} from "./harness";
import { browser, expect, test } from "../webview";

const pages: Attempt[][] = [];

test(
  "T1: after 2 s of lost UDP and beside a same-address neighbour, later WebTransport runs complete unrefused",
  async (page) => {
    const fleet = await shapedFleet([link(20, 200)], { signIn: true });
    try {
      await openApp(page, fleet, runConfig("transport:webtransport"));
      fleet.neighbour(`${fleet.servers[0]!.h3}/probe`);
      const runs: Attempt[] = [];
      pages.push(runs);
      for (const gap of [5, 14, 20]) {
        await Bun.sleep(gap * 1000);
        const run = await start(page, fleet);
        // Into the download, which a run of today's Firefox may already have failed.
        if (!runs.length) {
          await Bun.sleep(4500);
          await fleet.udpOutage(fleet.servers[0]!, 2000);
        }
        runs.push(await run.end());
      }
      const later = runs.slice(1);
      expect(later.map(measured)).toEqual(later.map(() => ({ outcome: "complete", failures: [] })));
      expect(runs.map((run) => run.servers[0]!.refused)).toEqual(runs.map(() => 0));
      const failures = runs.flatMap((run) => measured(run).failures);
      expect(failures.filter((failure) => / server-(busy|capacity):/.test(failure))).toEqual([]);
    } finally {
      await fleet.close();
    }
  },
  {
    timeout: 300_000,
    failing:
      browser === "firefox"
        ? "until step 3: QUIC connections abandoned in the outage hold the server's per-address quota"
        : undefined,
  },
);

budgetTest("T1's runs", pages);
