import { expect, test } from "bun:test";
import { json } from "../../protocol/pins.testutil";
import { Aggregate, type ThroughputResult } from "./aggregate";

const reported = (result: ThroughputResult | null) =>
  result && { bytesPerSec: result.reportedBytesPerSec, peakBytesPerSec: result.peakBytesPerSec };

test("the coordinated throughput the terminal clients share", async () => {
  for (const vector of await json("aggregation.testvectors.json")) {
    const aggregate = new Aggregate();
    aggregate.begin(vector.stage, vector.participants, vector.boundaries[0].atMs);
    let live: string[] = vector.participants;
    const downloaded = new Map<string, number>();
    for (const boundary of vector.boundaries) {
      if (boundary.dropout) {
        live = live.filter((id) => !boundary.dropout.includes(id));
        aggregate.dropout(live, boundary.atMs);
      }
      const up = Object.fromEntries(
        Object.entries(boundary.up).map(([id, r]: [string, any]) => [id, r && { ...r, receivedAtMs: boundary.atMs }]),
      );
      aggregate.observe({ atMs: boundary.atMs, down: boundary.down, up }, boundary.final);
      for (const [id, seen] of Object.entries<any>(boundary.observedUp ?? {}))
        aggregate.credit(id, { id: seen.id, bytes: seen.maximum });
      // The browser credits downloads as lanes report them; the vectors credit a counter's new high-water mark.
      for (const [id, bytes] of Object.entries<number>(boundary.down)) {
        const high = downloaded.get(id) ?? bytes;
        aggregate.addDownload(vector.stage, id, bytes - high);
        downloaded.set(id, Math.max(high, bytes));
      }
    }
    const totals = { down: 0, up: 0 };
    for (const id of vector.participants) {
      const each = aggregate.totals(id);
      totals.down += each.down;
      totals.up += each.up;
    }
    expect(totals, vector.name).toEqual(vector.totalBytes);
    const intervals = aggregate.intervals.map((interval) => ({
      reason: interval.reason,
      complete: interval.complete,
      window: interval.full && {
        startMs: interval.full.startMs,
        endMs: interval.full.endMs,
        downBytesPerSec: interval.full.downBytesPerSec,
        upBytesPerSec: interval.full.upBytesPerSec,
      },
    }));
    expect(intervals, vector.name).toEqual(vector.intervals);
    const { down, up } = aggregate.settle(vector.stage, false);
    expect({ down: reported(down?.result ?? null), up: reported(up?.result ?? null) }, vector.name).toEqual(
      vector.result,
    );
    for (const [id, expected] of Object.entries<any>(vector.servers ?? {}))
      expect(
        {
          down: reported(aggregate.serverResult(vector.stage, "down", id)),
          up: reported(aggregate.serverResult(vector.stage, "up", id)),
        },
        `${vector.name}: ${id}`,
      ).toEqual(expected);
  }
});
