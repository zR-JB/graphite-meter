// Section 7.5: a server whose real capacity another client holds is named at Start, before anything is measured.
import { attempt, capped, link, measured, openApp, runConfig, shapedFleet } from "./harness";
import { expect, test } from "../webview";

test(
  "T6: a server at capacity leaves the run as server-capacity before the timeline starts",
  async (page) => {
    // Another client's downloads hold every handler of S2.
    const handlers = 8;
    const fleet = await shapedFleet([link(20, 100), link(20, 100)], {
      env: [{}, capped(handlers)],
    });
    try {
      await openApp(page, fleet, runConfig("auto"));
      fleet.neighbour(`${fleet.servers[1]!.http}/download?bytes=${2 ** 40}`, {
        requests: handlers,
        everyMs: 50,
      });
      await Bun.sleep(2000);
      const run = await attempt(page, fleet);
      const { participants, failures } = run.record!.result.multiServer;
      expect(participants).toEqual(["self"]);
      expect(failures.map(({ serverId, reason }) => ({ serverId, reason }))).toEqual([
        { serverId: "s2", reason: "server-capacity" },
      ]);
      expect(measured(run).outcome).toBe("partial");
    } finally {
      await fleet.close();
    }
  },
  {
    timeout: 120_000,
    failing: "until step 9: capacity is found per lane mid-stage, without a lease",
  },
);
