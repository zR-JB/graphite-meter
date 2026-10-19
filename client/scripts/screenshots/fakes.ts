// The screenshot build's network and clock: fixed scenarios and timer-to-timer time, so runs pass alike at once.
import type { ProbeOutcome } from "../../src/protocol/messages";
import type { Clock } from "../../src/engine/run/clock";
import type {
  LatencyBus,
  OpenProbeChannel,
  OpenStage,
  StageResources,
} from "../../src/engine/types";

export * from "../../src/engine/run/clock";

/** A latency population: its median, the spread around it and its extremes, in ms. */
export interface Spread {
  median: number;
  jitter: number;
  min: number;
  max: number;
}

/** One run as the fakes report it; capture.ts sets it on the page before each run. */
export interface Scenario {
  epoch: number;
  /** Each server's payload rates, in bytes per second, and how far they wander from them. */
  down: number;
  up: number;
  wander: number;
  /** How far the download's warmup runs over its rate, which only the live dial sees. */
  overshoot: number;
  idle: Spread;
  loaded: { download: Spread; upload: Spread };
  /** Probe replies per second while measured. */
  replies: { idle: number; loaded: number };
  /** Each origin's round trip and preflight time as the server checks measure them. */
  rtt: Record<string, number>;
}

export const scenario = (): Scenario => Reflect.get(globalThis, "__gmScenario");

/** An origin's round trip; one the scenario does not name would measure the real network. */
export function rttOf(origin: string): number {
  const ms = scenario().rtt[origin];
  if (ms === undefined) throw new Error(`the scenario has no round trip for ${origin}`);
  return ms;
}

interface Timer {
  at: number;
  seq: number;
  fn: () => void;
}
const timers: Timer[] = [];
let now = 0;
let seq = 0;
let pending = false;
// A message is the next task, so every promise a timer settles runs before the clock moves on.
const turn = new MessageChannel();
turn.port1.onmessage = () => {
  pending = false;
  let next: Timer | undefined;
  for (const timer of timers)
    if (!next || timer.at < next.at || (timer.at === next.at && timer.seq < next.seq)) next = timer;
  if (!next) return;
  timers.splice(timers.indexOf(next), 1);
  now = Math.max(now, next.at);
  next.fn();
  schedule();
};
function schedule() {
  if (pending || !timers.length) return;
  pending = true;
  turn.port2.postMessage(0);
}
function after(ms: number, fn: () => void) {
  const timer = { at: now + Math.max(0, ms), seq: seq++, fn };
  timers.push(timer);
  schedule();
  return () => {
    const index = timers.indexOf(timer);
    if (index >= 0) timers.splice(index, 1);
  };
}
function every(ms: number, fn: () => void) {
  let cancel = after(ms, function tick() {
    cancel = after(ms, tick);
    fn();
  });
  return () => cancel();
}

export const pageClock: Clock = { now: () => now, epoch: () => scenario().epoch + now, after };

/** Uniform numbers in [0, 1) from a seed, the same on every machine (mulberry32). */
function random(seed: number) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function rtts(spread: Spread, seed: number) {
  const next = random(seed);
  return () => {
    const tail = next();
    if (tail < 0.003) return spread.median + (spread.max - spread.median) * next();
    if (tail < 0.006) return spread.min + (spread.median - spread.min) * next();
    const around = spread.median + spread.jitter * (next() + next() + next() - 1.5) * 2;
    return Math.round(Math.min(spread.max, Math.max(spread.min, around)) * 1000) / 1000;
  };
}

const TICK_MS = 20;
const RECEIVER_MS = 100;
const PROBE_MS = 10;

export const openStage: OpenStage = ({ activity, events, now: runMs }) => {
  const plan = scenario();
  // The rate holds each level a while, as a link's does, and each level's mirror follows it, so the mean stays the scenario's.
  const rate = (dir: "down" | "up", ms: number) => {
    const level = Math.floor(ms / 1000);
    const offset = random(activity.stage.length * 7919 + (level >> 1))() - 0.5;
    return plan[dir] * (1 + plan.wander * (level & 1 ? -offset : offset));
  };
  const spread =
    activity.stage === "latency" ? plan.idle : plan.loaded[activity.stage as "download" | "upload"];
  const perTick =
    (activity.stage === "latency" ? plan.replies.idle : plan.replies.loaded) * (PROBE_MS / 1000);
  let stop = () => {};
  let started = 0;
  let sent = 0;
  const receiver = () => ({
    id: activity.stage,
    bytes: Math.round(sent),
    nanos: Math.round((now - started) * 1e6) + 1,
    receivedAtMs: runMs(),
  });
  const resources: StageResources = {
    async prepare() {
      if (activity.transfer.includes("down"))
        stop = every(TICK_MS, () =>
          events.warmupDownload(Math.round((plan.down * (1 + plan.overshoot) * TICK_MS) / 1000)),
        );
    },
    measure() {
      stop();
      started = now;
      const stops: (() => void)[] = [];
      if (activity.transfer.length)
        stops.push(
          every(TICK_MS, () => {
            const ms = now - started;
            if (activity.transfer.includes("down"))
              events.download(Math.round((rate("down", ms) * TICK_MS) / 1000));
            if (activity.transfer.includes("up")) {
              sent += (rate("up", ms) * TICK_MS) / 1000;
              if (ms % RECEIVER_MS < TICK_MS) events.receiver(receiver());
            }
          }),
        );
      if (activity.stage === "latency" || activity.loadedLatency) {
        const rtt = rtts(spread, activity.stage.length * 104729);
        let owed = 0;
        stops.push(
          every(PROBE_MS, () => {
            owed += perTick;
            const outcomes: ProbeOutcome[] = [];
            for (let i = 0; i < Math.floor(owed); i++)
              outcomes.push({
                rttMs: rtt(),
                timedOut: false,
                handlingMs: null,
                observedAt: now,
                inWindow: true,
              });
            owed -= outcomes.length;
            if (outcomes.length) events.probes(outcomes);
          }),
        );
      }
      stop = () => {
        for (const each of stops) each();
      };
    },
    checkpoint: async () => receiver(),
    countAt: async () => null,
    flush: async () => null,
    replaceUpload: async () => {},
    finish: async () => stop(),
    discard: () => stop(),
  };
  return resources;
};

/** Probes that answer every interval at the origin's round trip, on the page's own timers. */
export const openProbeChannel: OpenProbeChannel = ({ target, intervalMs }, events): LatencyBus => {
  const rttMs = rttOf(target.origin);
  let interval = intervalMs ?? rttMs;
  let answered = false;
  let timer = setTimeout(function reply() {
    if (!answered) events.ready();
    answered = true;
    events.outcomes([
      { rttMs, timedOut: false, handlingMs: null, observedAt: performance.now(), inWindow: true },
    ]);
    timer = setTimeout(reply, interval);
  }, rttMs);
  return {
    measure() {},
    pace: (ms) => void (interval = ms ?? rttMs),
    stop: async () => {},
    dispose: () => clearTimeout(timer),
    adopt(next) {
      events = next;
      if (answered) next.ready();
      return true;
    },
  };
};
