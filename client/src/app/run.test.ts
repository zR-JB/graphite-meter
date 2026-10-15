import { expect, spyOn, test } from "bun:test";
import { Run } from "../engine/run/run";
import { FakeClock, prepared, resources, type Peer } from "../engine/run/testkit";
import type { TestConfig } from "../engine/types";
import { computed, effect, signal } from "../lib/signal";
import { createPreferences, DEFAULT_CONFIG } from "./preferences";
import { createRun } from "./run";

function setup(
  config: Partial<TestConfig> = {},
  options: { authorize?: Promise<void>; servers?: Peer[]; fits?: (ms: number) => boolean } = {},
) {
  const clock = new FakeClock();
  const preferences = createPreferences(null, new EventTarget());
  preferences.set({ config: { ...DEFAULT_CONFIG, adaptive: false, ...config } });
  const refusal = signal("");
  const offers: string[] = [];
  const calls: string[] = [];
  const peers = options.servers ?? [{ id: "self" }];
  const run = createRun({
    preferences,
    blocker: signal(""),
    refusal,
    authorize: () => options.authorize ?? Promise.resolve(),
    prepare: async () => ({ servers: peers.map(({ id }) => prepared(id, true)), dropped: [], focus: "self" }),
    fits: (ms) => options.fits?.(ms) ?? true,
    recheck: () => {},
    finished: (finished) => offers.push(finished ? "offer" : "withdraw"),
    open: (stage) => resources(peers.find(({ id }) => id === stage.server.server.id)!, stage, calls, clock),
    streams: (servers, activity) =>
      new Map(
        servers.map(({ server }) => [
          server.id,
          { streams: { down: +activity.transfer.includes("down"), up: +activity.transfer.includes("up") } },
        ]),
      ),
    clock,
  });
  return { clock, run, preferences, refusal, offers, calls };
}

const only = (stages: Partial<TestConfig["stages"]>, duration: Partial<TestConfig["duration"]> = {}) => ({
  stages: { latency: false, download: false, upload: false, bidirectional: false, ...stages },
  duration: { ...DEFAULT_CONFIG.duration, warmupMs: 0, ...duration },
});

test("a Start never looks idle on its way to running", async () => {
  const h = setup(only({ download: true }, { downloadMs: 1_000 }));
  const steps: string[] = [];
  const busy: boolean[] = [];
  effect(() => void steps.push(h.run.preparation()));
  const working = computed(() => h.run.running() || h.run.preparing());
  effect(() => void busy.push(working()));
  h.run.toggle();
  await h.clock.advance(100);
  expect(steps).toEqual(["idle", "authenticating", "checking", "launching", "idle"]);
  expect(busy).toEqual([false, true]);
});

test("disposing during a pending start launches nothing", async () => {
  const { promise, resolve } = Promise.withResolvers<void>();
  const h = setup(only({ download: true }, { downloadMs: 1_000 }), { authorize: promise });
  h.run.toggle();
  h.run.dispose();
  resolve();
  await h.clock.advance(2_000);
  expect(h.run.phase()).toBe("idle");
  expect(h.calls).toEqual([]);
});

test("an outright refusal says why where Start was pressed, and lapses with its cause", () => {
  const h = setup();
  h.refusal.set("Servers are still loading. Try again in a moment.");
  h.run.toggle();
  expect([h.run.preparation(), h.run.startError()]).toEqual([
    "blocked",
    "Servers are still loading. Try again in a moment.",
  ]);
  h.refusal.set("");
  expect([h.run.preparation(), h.run.startError()]).toEqual(["idle", ""]);
});

test("a start that prepares no server fails with words", async () => {
  const h = setup(only({ download: true }), { servers: [] });
  h.run.toggle();
  await h.clock.advance(10);
  expect([h.run.preparation(), h.run.startError()]).toEqual(["failed", "Connection check failed"]);
});

test("planning upload traffic mid-run needs receiver checkpoints, and a refusal changes nothing", async () => {
  const h = setup(only({ download: true }, { downloadMs: 2_000 }));
  h.run.toggle();
  await h.clock.advance(500);
  expect(h.run.toggleStage("bidirectional")).toBe(false);
  expect(h.run.refusal()).toContain("Receiver checkpoint support is required for uploads");
  expect(h.run.startError()).toBe("");
  expect(h.preferences.config().stages.bidirectional).toBe(false);
  expect(h.run.plan()?.stages.bidirectional).toBe(false);
});

test("a gauge-only change mid-run never reaches the run", async () => {
  const reconfigure = spyOn(Run.prototype, "reconfigure");
  try {
    const h = setup(only({ download: true }, { downloadMs: 2_000 }));
    h.run.toggle();
    await h.clock.advance(500);
    expect(h.run.configure({ visualization: { throughputMaxBytesPerSec: 125e6 } })).toBe(true);
    expect(reconfigure).not.toHaveBeenCalled();
  } finally {
    reconfigure.mockRestore();
  }
});

test("lengthening a live test must fit the session for the time left, not the time run", async () => {
  const asked: number[] = [];
  const h = setup(only({ download: true }, { downloadMs: 10_000 }), { fits: (ms) => (asked.push(ms), true) });
  h.run.toggle();
  await h.clock.advance(6_000);
  expect(h.run.configure({ duration: { ...h.preferences.config().duration, downloadMs: 12_000 } })).toBe(true);
  expect(asked).toHaveLength(1);
  expect(Math.abs(asked[0]! - (6_000 + 60_000))).toBeLessThan(200);
});

test("a later stage's bytes count from its own start", async () => {
  const h = setup(only({ download: true, upload: true }, { downloadMs: 1_000, uploadMs: 1_000 }));
  let first: number | null = null;
  effect(() => {
    if (h.run.stage() === "upload" && h.run.live()) first ??= h.run.stageBytes();
  });
  h.run.toggle();
  await h.clock.advance(4_000);
  expect(h.run.phase()).toBe("complete");
  expect(first).not.toBeNull();
  expect(first!).toBeLessThan(200);
});

test("a finished run is offered to History once, and the next start withdraws it", async () => {
  const h = setup(only({ download: true }, { downloadMs: 1_000 }));
  h.run.toggle();
  await h.clock.advance(3_000);
  expect(h.offers).toEqual(["withdraw", "offer"]);
  h.run.toggle();
  await h.clock.advance(100);
  expect(h.offers).toEqual(["withdraw", "offer", "withdraw"]);
});

test("the gauge maximum changes while a start prepares, and the start goes on", async () => {
  const { promise, resolve } = Promise.withResolvers<void>();
  const h = setup(only({ download: true }, { downloadMs: 1_000 }), { authorize: promise });
  h.run.toggle();
  expect(h.run.configure({ visualization: { throughputMaxBytesPerSec: 125e6 } })).toBe(true);
  expect(h.run.configure({ adaptive: true })).toBe(false);
  resolve();
  await h.clock.advance(3_000);
  expect(h.run.phase()).toBe("complete");
  expect(h.preferences.config().visualization.throughputMaxBytesPerSec).toBe(125e6);
});

test("a refusal speaks only for the change that caused it", async () => {
  const h = setup(only({ download: true }, { downloadMs: 2_000 }));
  h.run.toggle();
  await h.clock.advance(500);
  expect(h.run.toggleStage("bidirectional")).toBe(false);
  expect(h.run.refusal()).not.toBe("");
  expect(h.run.configure({ adaptive: true })).toBe(true);
  expect(h.run.refusal()).toBe("");
});
