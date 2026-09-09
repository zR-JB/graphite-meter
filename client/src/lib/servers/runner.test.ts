import { expect, test } from "bun:test";
import type { CoreHost } from "../runner/core";
import type { RunnerEvent, RunResult } from "../runner/contract";
import { DEFAULT_CONFIG } from "../state/defaults";
import {
  testPreparedPaths,
  TEST_BUILD_TOKENS,
} from "../runner/test-helpers.test";
import { stubGlobals } from "../test-helpers.test";
import { buildHistoryRecord, isHistoryRecord } from "../history/types";

test("one selected server delivers direct samples and visual hints without aggregate checkpoints", async () => {
  const restore = stubGlobals(TEST_BUILD_TOKENS);
  const { createServerRunner } = await import("./runner");
  let host: CoreHost;
  let timer: ReturnType<typeof setInterval> | undefined;
  let checkpoints = 0;
  const events: RunnerEvent[] = [];
  const paths = testPreparedPaths({ latency: null });
  const server = { id: "remote", name: "Remote", url: "http://meter.test" };
  const stop = () => clearInterval(timer);
  const runner = createServerRunner([{ server, paths }], server.id, () => ({
    attach(value) {
      host = value;
    },
    onRunStart() {},
    onStageBegin() {},
    onStageMeasure() {
      let last = performance.now();
      timer = setInterval(() => {
        const now = performance.now();
        const seconds = (now - last) / 1000;
        last = now;
        host.ingestThroughput("down", seconds * 1000, seconds);
        host.ingestThroughput("up", seconds * 2000, seconds, true);
        host.emit({ type: "uploadPresentation", bytesPerSec: 999999 });
      }, 20);
    },
    onStageEnd: stop,
    onAbort: stop,
    onComplete: stop,
    flushDownload() {},
    async checkpoint() {
      checkpoints++;
      throw new Error("unexpected aggregate checkpoint");
    },
  }));
  try {
    const completion = new Promise<RunResult>((resolve, reject) =>
      runner.on((event) => {
        events.push(event);
        if (event.type === "complete") resolve(event.result);
        if (event.type === "error") reject(event.error);
      }),
    );
    runner.start(
      {
        ...structuredClone(DEFAULT_CONFIG),
        stages: {
          latency: false,
          download: false,
          upload: false,
          bidirectional: true,
        },
        skipLoadedLatencyWhenStageOff: true,
        duration: {
          ...DEFAULT_CONFIG.duration,
          warmupMs: 0,
          bidirectionalMs: 900,
        },
        adaptive: { ...DEFAULT_CONFIG.adaptive, enabled: false },
      },
      0,
    );
    const result = await completion;
    expect(checkpoints).toBe(0);
    expect(
      events.filter(
        (event) => event.type === "throughput" && event.sample.dir === "down",
      ).length,
    ).toBeGreaterThan(6);
    expect(
      events.some(
        (event) =>
          event.type === "uploadPresentation" && event.bytesPerSec === 999999,
      ),
    ).toBe(true);
    expect(result.bidirectional!.down!.reportedBytesPerSec).toBeCloseTo(1000);
    expect(result.bidirectional!.up!.reportedBytesPerSec).toBeCloseTo(2000);
    expect(result.bidirectional!.up!.serverAuthoritative).toBe(true);
    expect(result.multiServer!.selection).toEqual([server]);
    expect(result.multiServer!.intervals).toEqual([]);
    const record = buildHistoryRecord(result, { paths, clientBuild: "test" });
    expect(isHistoryRecord(JSON.parse(JSON.stringify(record)))).toBe(true);
    expect(record.totalBytes).toBe(
      result.bidirectional!.down!.totalBytes +
        result.bidirectional!.up!.totalBytes,
    );
  } finally {
    runner.dispose();
    restore();
  }
});
