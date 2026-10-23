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
        // A second into the download, or once the run ended, as today's Firefox may fail it before it starts.
        if (!runs.length) {
          await expect
            .poll(
              () =>
                page.evaluate(() => document.querySelector("#console")?.getAttribute("data-phase")),
              {
                timeout: 20_000,
              },
            )
            .toMatch(/^(download|upload|complete|error|aborted)$/);
          await Bun.sleep(1000);
          await fleet.udpOutage(fleet.servers[0]!, 2000);
        }
        runs.push(await run.end());
      }
      const later = runs.slice(1);
      expect(later.map(measured)).toEqual(later.map(() => ({ outcome: "complete", failures: [] })));
      const refused = runs.map(({ servers: [server] }) => server!.refused + server!.quicRefused);
      expect(refused).toEqual(runs.map(() => 0));
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
