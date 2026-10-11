import { expect, test } from "bun:test";
import type { ProbeReport } from "../../protocol/messages";
import type { ProbeOutcome } from "../types";
import { Prober } from "./prober";

/** A probe's fate: a reply after so many ms, none, a send that throws, or a write that never confirms. */
type Fate = number | "lost" | "unsent" | "unconfirmed";

/** A bus on a fake clock. At one instant replies run before the timer. */
function bus(intervalMs: number | null, maxInFlight: number, fate: (id: number) => Fate) {
  const clock = { now: 0, due: null as number | null, dials: 0, wakes: 0 };
  const sent: number[] = [];
  const replies: { id: number; at: number }[] = [];
  const reports: ProbeReport[] = [];
  const stamps: [number, ProbeReport["type"]][] = [];
  const prober = new Prober(
    { intervalMs, maxInFlight },
    {
      origin: 0,
      now: () => clock.now,
      arm: (ms) => (clock.due = clock.now + ms),
      disarm: () => (clock.due = null),
    },
    {
      dial: () => clock.dials++,
      send(text) {
        const id = Number(text.slice("PING,".length));
        sent.push(clock.now);
        const kind = fate(id);
        if (kind === "unsent") throw new Error("not open");
        if (kind === "unconfirmed") return new Promise<void>(() => {});
        if (kind !== "lost") replies.push({ id, at: clock.now + kind });
      },
      hangUp() {},
      post(report) {
        reports.push(report);
        stamps.push([clock.now, report.type]);
      },
    },
  );
  const run = (until: number) => {
    for (;;) {
      const reply = replies.reduce<(typeof replies)[number] | null>((a, b) => (a && a.at <= b.at ? a : b), null);
      const at = Math.min(reply?.at ?? Infinity, clock.due ?? Infinity);
      if (at > until) break;
      clock.now = at;
      if (reply?.at === at) {
        replies.splice(replies.indexOf(reply), 1);
        prober.frame(`PONG,${reply.id},0`, at);
      } else {
        clock.due = null;
        clock.wakes++;
        prober.wake();
      }
    }
    clock.now = until;
  };
  const of = <T extends ProbeReport["type"]>(type: T) =>
    reports.filter((report): report is Extract<ProbeReport, { type: T }> => report.type === type);
  const outcomes = () => of("outcomes").flatMap((report) => report.outcomes.map(pick));
  return { prober, clock, sent, reports, stamps, run, of, outcomes };
}

const pick = ({ rttMs, timedOut, observedAt, inWindow }: ProbeOutcome) => ({ rttMs, timedOut, observedAt, inWindow });

function opened(b: ReturnType<typeof bus>) {
  b.prober.start();
  b.prober.measure();
  b.prober.opened();
  return b;
}

test("a fixed cadence spaces sends from the actual sends: a fast reply never pulls one early, a late one never bursts", () => {
  for (const [cadence, sends] of [
    [80, 51],
    [250, 17],
    [600, 7],
  ] as const) {
    const b = opened(bus(cadence, 16, () => 0));
    b.run(4_000);
    expect(b.sent.length, `${cadence} ms`).toBe(sends);
    expect(b.sent.every((at, i) => !i || at - b.sent[i - 1]! >= cadence)).toBe(true);
  }
  // A full window skips a turn; the overdue probe goes when a reply frees the slot.
  const slow = opened(bus(80, 1, () => 200));
  slow.run(1_000);
  expect(slow.sent).toEqual([0, 200, 400, 600, 800, 1_000]);
});

test("a new pace spaces the next send from the last one, and the probe in flight still resolves", () => {
  const b = opened(bus(120, 2, (id) => (id === 1 ? 100 : 10)));
  b.run(150);
  b.prober.pace(1_000);
  b.run(2_500);
  expect(b.sent).toEqual([0, 120, 1_120, 2_120]);
  expect(b.outcomes().map(({ rttMs, timedOut }) => [rttMs, timedOut])).toEqual([
    [10, false],
    [100, false],
    [10, false],
    [10, false],
  ]);
});

test("a reply-driven chain steps on its latest probe, its backup covers a late reply, and a full window sleeps", () => {
  // The backup waits 250 ms before any reply; after a 10 ms one, 10 + 4 × 5 = 30 ms.
  const late = opened(bus(null, 4, (id) => (id === 1 ? 100 : 10)));
  late.run(115);
  // Probe 1 is late, so its backup sends probe 2 at 40; its own reply at 110 steps nothing.
  expect(late.sent).toEqual([0, 10, 40, 50, 60, 70, 80, 90, 100, 110]);
  late.run(180);
  expect(late.outcomes().map(({ rttMs }) => rttMs)).toEqual([10, 10, 10, 10, 10, 10, 10, 100, ...Array(8).fill(10)]);
  // With both slots unanswered the chain waits for its head, which only its own deadline resolves.
  const full = opened(bus(null, 2, (id) => (id ? "lost" : 10)));
  full.run(300);
  expect(full.sent).toEqual([0, 10, 40, 290]);
  full.run(310);
  expect(full.outcomes()).toEqual([
    { rttMs: 10, timedOut: false, observedAt: 10, inWindow: true },
    { rttMs: 250, timedOut: true, observedAt: 260, inWindow: true },
    { rttMs: 250, timedOut: true, observedAt: 290, inWindow: true },
  ]);
});

test("deadlines are fixed at send; a late reply stays a timeout yet teaches the estimator; handling pairs only when possible", () => {
  const b = opened(bus(1_000, 16, (id) => (id === 2 ? "lost" : 300)));
  b.run(2_010);
  // The 250 ms deadline times probe 0 out; its late reply lifts the next deadline to 300 + 4 × 150 ms.
  for (const reply of ["PONG,2", "PONG,2,1,2", "PONG,2,20000000", "PONG,2,0"]) b.prober.frame(reply, 2_010);
  b.run(2_100);
  expect(b.outcomes()).toEqual([
    { rttMs: 250, timedOut: true, observedAt: 250, inWindow: true },
    { rttMs: 300, timedOut: false, observedAt: 1_300, inWindow: true },
    { rttMs: 10, timedOut: false, observedAt: 2_010, inWindow: true },
  ]);
  // 20 ms of handling cannot fit a 10 ms reply: the reply stays, the pair is dropped, never clamped.
  expect(b.of("outcomes").flatMap((report) => report.outcomes.map(({ handlingMs }) => handlingMs))).toEqual([
    null,
    0,
    null,
  ]);
  expect(b.of("ready")).toHaveLength(1);
});

test("a stop drains measured probes to their own deadlines outside the window, then acknowledges", () => {
  const b = opened(bus(20, 4, (id) => ([5, 100, "lost", "unconfirmed"] as const)[id] ?? 5));
  b.run(65);
  const before = b.reports.length;
  b.prober.stop();
  b.run(400);
  expect(b.sent).toEqual([0, 20, 40, 60]);
  expect(b.stamps.slice(before)).toEqual([
    [170, "outcomes"],
    [310, "outcomes"],
    [310, "interrupted"],
    [310, "stopped"],
  ]);
  expect(b.outcomes().slice(1)).toEqual([
    { rttMs: 100, timedOut: false, observedAt: 120, inWindow: false },
    { rttMs: 250, timedOut: true, observedAt: 290, inWindow: false },
  ]);
  // A write never confirmed stays unresolved when the drain ends at the last deadline.
  expect(b.of("interrupted")).toEqual([{ type: "interrupted", count: 1, reason: "unresolved" }]);
  b.prober.stop();
  expect(b.reports.at(-1)).toEqual({ type: "stopped" });
});

test("a disconnect keeps passed deadlines as timeouts, interrupts the rest, stalls once per outage and backs off", () => {
  const b = opened(bus(100, 16, (id) => (id < 3 ? "lost" : 5)));
  b.run(249);
  b.clock.now = 250;
  b.prober.closed("websocket closed");
  expect(b.reports.map(({ type }) => type)).toEqual(["outcomes", "interrupted", "stalled"]);
  expect(b.outcomes()).toEqual([{ rttMs: 250, timedOut: true, observedAt: 250, inWindow: true }]);
  expect(b.of("interrupted")).toEqual([{ type: "interrupted", count: 2, reason: "unresolved" }]);
  const dialAt = (at: number) => {
    const dials = b.clock.dials;
    b.run(at - 1);
    expect(b.clock.dials, `${at}`).toBe(dials);
    b.run(at);
    expect(b.clock.dials, `${at}`).toBe(dials + 1);
  };
  for (const at of [350, 550, 950, 1_750, 3_350, 5_350]) {
    dialAt(at);
    b.prober.closed("refused");
  }
  dialAt(7_350);
  b.prober.opened();
  // Malformed and unknown replies establish nothing.
  b.prober.frame("PONG,99,0", 7_350);
  b.prober.frame("ready", 7_350);
  expect(b.of("ready")).toHaveLength(0);
  b.run(7_360);
  expect(b.reports.slice(-2).map(({ type }) => type)).toEqual(["ready", "resumed"]);
  expect(b.of("stalled")).toHaveLength(1);
  // An opened connection resets the backoff, and its loss is a new outage.
  b.prober.closed("websocket closed");
  expect(b.of("stalled")).toHaveLength(2);
  dialAt(7_460);
});

test("a send the browser could not make is interrupted after the replies already observed, never a timeout", () => {
  const b = opened(bus(50, 16, (id) => (id === 1 ? "unsent" : 5)));
  b.run(200);
  expect(b.stamps).toEqual([
    [5, "ready"],
    [50, "outcomes"],
    [50, "interrupted"],
    [155, "outcomes"],
  ]);
  expect(b.of("interrupted")).toEqual([{ type: "interrupted", count: 1, reason: "send-failed" }]);
  expect(b.outcomes().every(({ timedOut }) => !timedOut)).toBe(true);
  expect(b.sent).toEqual([0, 50, 100, 150, 200]);
});

test("outcomes cross in batches of at most 128 within 50 ms, and an idle bus wakes only to probe and report", () => {
  /** How long after its observation each outcome crossed. */
  const lags = ({ reports, stamps }: ReturnType<typeof bus>) =>
    reports.flatMap((report, i) =>
      report.type === "outcomes" ? report.outcomes.map(({ observedAt }) => stamps[i]![0] - observedAt) : [],
    );
  const dense = opened(bus(null, 4, () => 0.01));
  dense.run(10);
  dense.prober.stop();
  dense.run(11);
  const batches = dense.of("outcomes").map(({ outcomes }) => outcomes.length);
  expect(batches.slice(0, -1).every((size) => size === 128)).toBe(true);
  expect(batches.reduce((sum, size) => sum + size)).toBe(dense.sent.length);
  expect(lags(dense).every((lag) => lag <= 50)).toBe(true);
  expect(dense.reports.at(-1)).toEqual({ type: "stopped" });
  const idle = opened(bus(1_000, 2, () => 1));
  idle.run(10_000);
  // A lone reply waits for the batch, never past it; each second one wake sends and one reports.
  expect(lags(idle)).toEqual(Array(10).fill(50));
  expect(idle.clock.wakes).toBeLessThanOrEqual(21);
});
