// The application, wired once at boot: preferences, the servers and their checks, the run, History saving, and the
// idle latency the console shows between runs. Views read its signals and call its methods.
import { planStreams } from "../engine/net/budget";
import { readPageConfig, createPageSession } from "../engine/net/page";
import { createServers } from "../engine/net/servers";
import { openProbeChannel } from "../engine/lanes/probes";
import { openStage } from "../engine/lanes/stage";
import type { LatencyBucket } from "../engine/measure/buckets";
import { adaptWarmup, totalMs } from "../engine/run/schedule";
import { pageMs } from "../lib/frame";
import { computed, effect, signal, untrack } from "../lib/signal";
import { historyRecord } from "./history/records";
import { HistoryRepository } from "./history/repository";
import { createHistorySaving } from "./history/saving";
import { createPreferences, savingResults } from "./preferences";
import { createRun } from "./run";
import { IDLE_LIMIT, idleBucket, upsertLatency } from "./series";
import { stageLimitError } from "../view/words";

/** Ending for a lapsed login waits this long for the result to reach History. */
const SIGN_OUT_SAVE_MS = 3_000;
const SAVE_POLL_MS = 50;

export type App = ReturnType<typeof createApp>;

/** Storage that is absent or blocked reads as none. */
const local = () => {
  try {
    return localStorage;
  } catch {
    return null;
  }
};

export function createApp() {
  const preferences = createPreferences(local(), window);
  const page = readPageConfig();
  const session = createPageSession(page, pageMs);
  const saving = computed(() => savingResults(preferences.resultHistoryPreference(), page.historyDefault));
  const history = new HistoryRepository();
  const saver = createHistorySaving(history, saving, window);

  const booted = signal(false);
  /** A lapsed login is being left: nothing checks or probes against it any more. */
  const leaving = signal(false);
  const visible = signal(document.visibilityState === "visible");
  const online = signal(navigator.onLine);
  const latencyPin = signal<string | null>(null);
  const idle = { server: null as string | null, buckets: [] as LatencyBucket[], version: signal(0) };
  const clearIdle = () => {
    idle.buckets.length = 0;
    idle.version.set(idle.version.peek() + 1);
  };
  let busy = () => false;

  const servers = createServers({
    page,
    session,
    config: preferences.config,
    setTransports: (transports) => preferences.set({ config: { ...preferences.config.peek(), transports } }),
    // Nothing reads the run before boot, which comes after it exists.
    active: computed(() => booted() && visible() && !leaving() && !busy()),
    online,
    latencyPin,
    openProbeChannel,
    idleLatency(id, outcomes) {
      if (id !== idle.server) {
        idle.server = id;
        idle.buckets.length = 0;
      }
      for (const { observedAt, rttMs, timedOut } of outcomes)
        upsertLatency(idle.buckets, idleBucket(observedAt, rttMs, timedOut), IDLE_LIMIT);
      idle.version.set(idle.version.peek() + 1);
    },
    now: pageMs,
  });

  // What Start waits for: the servers, then each planned stage within every server's limit, then lanes that fit.
  const blocker = computed(
    () => servers.blocker() || stageLimitError(preferences.config(), servers.stageLimit()) || servers.streamPlanError(),
  );
  const run = createRun({
    preferences,
    blocker,
    refusal: computed(() => {
      if (servers.loading()) return "Servers are still loading. Try again in a moment.";
      const { state } = servers.approval();
      return state === "preparing" || state === "waiting"
        ? "Finish signing in to the selected server before starting."
        : "";
    }),
    authorize: servers.authorize,
    prepare: servers.prepare,
    fits: servers.fits,
    recheck: servers.recheck,
    finished: (finished) => (finished ? saver.offer(historyRecord(finished)) : saver.withdraw()),
    open: openStage,
    streams: (prepared, activity) =>
      new Map(
        [...planStreams(prepared, activity, preferences.config.peek().transferStreams)].map(([id, { down, up }]) => [
          id,
          { streams: { down: down.planned, up: up.planned }, gating: { down: down.gating, up: up.gating } },
        ]),
      ),
  });
  busy = computed(() => run.running() || run.preparing());
  const plannedMs = computed(() => {
    const plan = run.running() ? run.plan() : null;
    const rttMs = servers.views().get(servers.latencyServer() ?? "")?.latency.path?.rttMs ?? 0;
    return totalMs(plan ?? adaptWarmup(preferences.config(), rttMs));
  });
  // A run's latency replaces the idle series, and a newly shown server starts its own.
  const stopIdle = effect(() => {
    servers.latencyServer();
    run.running();
    untrack(clearIdle);
  });

  const onVisibility = () => {
    visible.set(document.visibilityState === "visible");
    if (visible.peek()) servers.resume();
  };
  const onOnline = () => online.set(navigator.onLine);
  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener("online", onOnline);
  window.addEventListener("offline", onOnline);

  let signingOut = false;
  /** Waits, at most a few seconds, until no run is live and the result shown is saved. */
  const settled = async () => {
    const deadline = pageMs() + SIGN_OUT_SAVE_MS;
    while ((run.running.peek() || saver.pending.peek()) && pageMs() < deadline)
      await new Promise((resolve) => setTimeout(resolve, SAVE_POLL_MS));
  };
  // A lapsed login ends the run, keeps what it can, and leaves for sign-in once.
  const stopSessionEnd = effect(() => {
    const reason = session.ended();
    if (!reason || signingOut) return;
    signingOut = true;
    untrack(() => {
      leaving.set(true);
      run.end("sign-in-required", "Signed out during the test");
    });
    void settled().then(() => {
      dispose();
      location.replace(`/login?reason=${reason}`);
    });
  });

  function dispose(): void {
    stopIdle();
    stopSessionEnd();
    document.removeEventListener("visibilitychange", onVisibility);
    window.removeEventListener("online", onOnline);
    window.removeEventListener("offline", onOnline);
    run.dispose();
    servers.dispose();
    saver.dispose();
    history.close();
    preferences.dispose();
  }

  return {
    page,
    session,
    preferences,
    servers,
    run,
    history,
    saver,
    saving,
    /** Why Start cannot run now, or "". */
    blocker,
    /** The running test's length, else the next one's, its warmup adapted to the latency server's round trip. */
    plannedMs,
    latencyPin,
    /** The latency server's idle probes between runs; `version` bumps when `buckets` changes in place. */
    idle: idle as { readonly buckets: readonly LatencyBucket[]; readonly version: typeof idle.version },
    boot() {
      if (booted.peek()) return;
      booted.set(true);
      void servers.load();
    },
    /** The sign-out form is submitting: the login it ends is no reason to leave for sign-in. */
    signOut(): void {
      signingOut = true;
    },
    dispose,
  };
}
