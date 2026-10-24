// Section 7.5: an upload belongs to its lease, so its control on the discovery origin owns what another address sends.
import { link, measured, openApp, runConfig, shapedFleet, start } from "./harness";
import { NETWORK_FLEET } from "../servers";
import { expect, test } from "../webview";

const REQUEST = /request:\s+(\S+) (\S+) from (\S+) on (\S+): (\d+)/;

test(
  "an upload whose control reaches the server from ::1 and its lanes from 127.0.0.1 is measured",
  async (page) => {
    const [server] = NETWORK_FLEET;
    // The page's clear HTTP origin is on ::1; its HTTP/2 origin, which the run measures over, on 127.0.0.11.
    const fleet = await shapedFleet([link(20, 100)], {
      env: [{ GM_H2_PUBLIC_ORIGIN: server!.h2 }],
    });
    try {
      await openApp(page, fleet, runConfig(server!.h2), server!.http);
      const mark = fleet.logs[0]!.mark();
      const run = await start(page, fleet);
      const ended = await run.end();
      expect(measured(ended)).toEqual({ outcome: "complete", failures: [] });
      const requests = fleet.logs[0]!.lines(mark).flatMap((line) => {
        const [, method, path, peer, endpoint, status] = REQUEST.exec(line) ?? [];
        return method ? [{ method, path, peer, endpoint, status }] : [];
      });
      const from = (path: string) =>
        new Set(requests.filter((each) => each.path === path).map(({ peer }) => peer));
      expect([from("/upload"), from("/upload/checkpoint"), from("/lease")]).toEqual([
        new Set(["127.0.0.1"]),
        new Set(["::1"]),
        new Set(["::1"]),
      ]);
      expect(requests.filter(({ status }) => status === "403")).toEqual([]);
    } finally {
      await fleet.close();
    }
  },
  { timeout: 120_000 },
);
