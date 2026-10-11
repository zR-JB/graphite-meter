// The browser's own settings in localStorage "graphite-meter:v1": the next run's test settings and display
// preferences. The key and its shape are shared with the old client, the pre-paint script and the sign-in pages.
import { normalizeStreamCount } from "../engine/net/budget";
import type { PingCadence, TestConfig } from "../engine/types";
import { batch, signal, type Read, type Signal } from "../lib/signal";
import { MAX_STAGE_LIMIT_MS } from "../protocol/documents";

export const STORAGE_KEY = "graphite-meter:v1";
const SAVE_DELAY_MS = 250;

export const DEFAULT_CONFIG: TestConfig = {
  stages: { latency: true, download: true, upload: true, bidirectional: false },
  skipLoadedLatencyWhenStageOff: true,
  duration: { warmupMs: 800, latencyMs: 4_000, downloadMs: 10_000, uploadMs: 10_000, bidirectionalMs: 10_000 },
  pingCadence: "reply-driven",
  loadedPingCadence: "medium",
  transferStreams: { mode: "auto", count: 4 },
  experimentalDatagramThroughput: false,
  transports: { throughputTarget: "auto", latencyTarget: "auto" },
  adaptive: true,
  visualization: { throughputMaxBytesPerSec: "auto" },
};

type DurationKey = keyof TestConfig["duration"];
export const DURATION_LIMITS: Record<DurationKey, readonly [number, number]> = {
  warmupMs: [0, 4_000],
  latencyMs: [1_000, MAX_STAGE_LIMIT_MS],
  downloadMs: [1_000, MAX_STAGE_LIMIT_MS],
  uploadMs: [1_000, MAX_STAGE_LIMIT_MS],
  bidirectionalMs: [1_000, MAX_STAGE_LIMIT_MS],
};

export const DURATION_PRESETS: Record<"short" | "medium" | "long", TestConfig["duration"]> = {
  short: { warmupMs: 600, latencyMs: 2_500, downloadMs: 5_000, uploadMs: 5_000, bidirectionalMs: 5_000 },
  medium: { warmupMs: 800, latencyMs: 4_000, downloadMs: 10_000, uploadMs: 10_000, bidirectionalMs: 10_000 },
  long: { warmupMs: 1_200, latencyMs: 6_000, downloadMs: 20_000, uploadMs: 20_000, bidirectionalMs: 20_000 },
};

/** A whole number of ms within the setting's limits; anything else is its default. */
export function clampDuration(key: DurationKey, value: unknown): number {
  const [min, max] = DURATION_LIMITS[key];
  return typeof value === "number" && Number.isFinite(value)
    ? Math.min(max, Math.max(min, Math.round(value)))
    : DEFAULT_CONFIG.duration[key];
}

export const HISTORY_COLUMNS = ["download", "upload", "bidirectional", "idle", "loaded"] as const;
export type HistoryColumn = (typeof HISTORY_COLUMNS)[number];
export type Theme = "dark" | "light" | "auto";
export type HistoryPreference = "default" | "enabled" | "disabled";

/** In the order they are stored. */
export interface Preferences {
  config: TestConfig;
  unitBase: "base10" | "base2";
  unitKind: "bits" | "bytes";
  theme: Theme;
  showWireEstimates: boolean;
  keyShortcuts: boolean;
  explanations: "hover" | "click";
  /** `default` follows the operator's choice. */
  resultHistoryPreference: HistoryPreference;
  historyColumns: HistoryColumn[];
  /** Docked sheet widths in px, clamped where they are used. */
  dockWidth: { left: number; right: number };
  /** The History list's share of its body width. */
  historySplit: number;
}

const record = (value: unknown): Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
const choice = <T extends string>(value: unknown, values: readonly T[], fallback: T): T =>
  values.includes(value as T) ? (value as T) : fallback;
const flag = (value: unknown, fallback: boolean) => (typeof value === "boolean" ? value : fallback);
const text = (value: unknown, fallback: string) =>
  typeof value === "string" && value.length <= 256 ? value : fallback;
const positive = <T>(value: unknown, fallback: T): number | T =>
  typeof value === "number" && Number.isFinite(value) && value >= 1 ? value : fallback;
const CADENCES: readonly PingCadence[] = ["reply-driven", "fast", "medium", "slow"];

/** Each stored field sanitised on its own: anything unreadable falls back to its default, unknown keys go. */
export function readPreferences(stored: unknown): Preferences {
  const saved = record(stored);
  const config = record(saved.config);
  const stages = record(config.stages);
  const duration = record(config.duration);
  // Earlier versions skipped a stage by giving it 0 ms; only its switch skips now.
  const skipped = (key: DurationKey) => key !== "warmupMs" && typeof duration[key] === "number" && duration[key] <= 0;
  const streams = record(config.transferStreams);
  const transports = record(config.transports);
  const dock = record(saved.dockWidth);
  const columns = Array.isArray(saved.historyColumns)
    ? [...new Set(saved.historyColumns.filter((column) => HISTORY_COLUMNS.includes(column)))]
    : [];
  const base = DEFAULT_CONFIG;
  const stage = (name: keyof TestConfig["stages"]) => flag(stages[name], base.stages[name]) && !skipped(`${name}Ms`);
  return {
    config: {
      stages: {
        latency: stage("latency"),
        download: stage("download"),
        upload: stage("upload"),
        bidirectional: stage("bidirectional"),
      },
      skipLoadedLatencyWhenStageOff: flag(config.skipLoadedLatencyWhenStageOff, base.skipLoadedLatencyWhenStageOff),
      duration: Object.fromEntries(
        (Object.keys(DURATION_LIMITS) as DurationKey[]).map((key) => [
          key,
          clampDuration(key, skipped(key) ? undefined : duration[key]),
        ]),
      ) as TestConfig["duration"],
      pingCadence: choice(config.pingCadence, CADENCES, base.pingCadence),
      loadedPingCadence: choice(config.loadedPingCadence, CADENCES, base.loadedPingCadence),
      transferStreams: {
        mode: choice(streams.mode, ["auto", "forced"], "auto"),
        count: typeof streams.count === "number" ? normalizeStreamCount(streams.count) : base.transferStreams.count,
      },
      experimentalDatagramThroughput: flag(config.experimentalDatagramThroughput, base.experimentalDatagramThroughput),
      transports: {
        throughputTarget: text(transports.throughputTarget, "auto"),
        latencyTarget: text(transports.latencyTarget, "auto"),
      },
      // Earlier versions saved { enabled }.
      adaptive: flag(record(config.adaptive).enabled ?? config.adaptive, base.adaptive),
      visualization: {
        throughputMaxBytesPerSec: positive(record(config.visualization).throughputMaxBytesPerSec, "auto" as const),
      },
    },
    unitBase: choice(saved.unitBase, ["base10", "base2"], "base10"),
    unitKind: choice(saved.unitKind, ["bits", "bytes"], "bits"),
    theme: choice(saved.theme, ["dark", "light", "auto"], "auto"),
    showWireEstimates: flag(saved.showWireEstimates, true),
    keyShortcuts: flag(saved.keyShortcuts, true),
    explanations: choice(saved.explanations, ["hover", "click"], "hover"),
    resultHistoryPreference: choice(saved.resultHistoryPreference, ["default", "enabled", "disabled"], "default"),
    historyColumns: columns.length ? columns : ["download", "upload", "idle", "loaded"],
    dockWidth: { left: positive(dock.left, 420), right: positive(dock.right, 420) },
    historySplit:
      typeof saved.historySplit === "number" && saved.historySplit >= 0 && saved.historySplit <= 1
        ? saved.historySplit
        : 0.42,
  };
}

export type PreferenceSignals = { readonly [K in keyof Preferences]: Read<Preferences[K]> };

export interface PreferenceStore extends PreferenceSignals {
  /** The one writer. */
  set(patch: Partial<Preferences>): void;
  dispose(): void;
}

/** Storage that is absent, blocked, full or corrupt never raises: a preference then lasts for this page only. */
function read(storage: Storage | null): unknown {
  try {
    const text = storage?.getItem(STORAGE_KEY);
    return text ? JSON.parse(text) : null;
  } catch {
    return null;
  }
}

/** Loads the preferences, saves changes once edits pause and before the page goes, and follows the History
    choices other tabs make. A save rewrites only the fields this tab changed over what is stored, so two tabs keep
    each other's changes to other fields (the test settings are one field); the first save stores the normalised
    whole. */
export function createPreferences(
  storage: Storage | null,
  target: Pick<Window, "addEventListener" | "removeEventListener">,
): PreferenceStore {
  const loaded = readPreferences(read(storage));
  const keys = Object.keys(loaded) as (keyof Preferences)[];
  const signals = Object.fromEntries(keys.map((key) => [key, signal(loaded[key])])) as {
    [K in keyof Preferences]: Signal<Preferences[K]>;
  };
  let dirty = new Set<keyof Preferences>(keys);
  let timer: ReturnType<typeof setTimeout> | undefined;

  const save = () => {
    clearTimeout(timer);
    timer = undefined;
    if (!dirty.size) return;
    const merged = readPreferences(read(storage)) as unknown as Record<string, unknown>;
    for (const key of dirty) merged[key] = signals[key].peek();
    // A refused write keeps its fields dirty, so the next save carries them.
    try {
      storage?.setItem(STORAGE_KEY, JSON.stringify(merged));
      dirty = new Set();
    } catch {}
  };
  const schedule = () => {
    clearTimeout(timer);
    timer = setTimeout(save, SAVE_DELAY_MS);
  };
  // Only History choices follow other tabs; an event that changes nothing writes nothing back.
  const follow = (event: StorageEvent) => {
    if (event.key !== STORAGE_KEY) return;
    const next = readPreferences(read(storage));
    batch(() => {
      // A choice this tab has not saved yet is the newer one.
      if (
        !dirty.has("resultHistoryPreference") &&
        next.resultHistoryPreference !== signals.resultHistoryPreference.peek()
      )
        signals.resultHistoryPreference.set(next.resultHistoryPreference);
      if (!dirty.has("historyColumns") && String(next.historyColumns) !== String(signals.historyColumns.peek()))
        signals.historyColumns.set(next.historyColumns);
    });
  };
  // A phone may discard a hidden tab without pagehide.
  const hidden = () => {
    if (document.visibilityState === "hidden") save();
  };
  target.addEventListener("storage", follow as EventListener);
  target.addEventListener("pagehide", save);
  target.addEventListener("visibilitychange", hidden);
  schedule();
  return {
    ...signals,
    set(patch) {
      batch(() => {
        for (const key of Object.keys(patch) as (keyof Preferences)[]) {
          (signals[key] as Signal<unknown>).set(patch[key]);
          dirty.add(key);
        }
      });
      schedule();
    },
    dispose() {
      save();
      target.removeEventListener("storage", follow as EventListener);
      target.removeEventListener("pagehide", save);
      target.removeEventListener("visibilitychange", hidden);
    },
  };
}

/** Saving is the browser's own choice once made, else the operator's. */
export const savingResults = (preference: HistoryPreference, operatorDefault: boolean) =>
  preference === "enabled" || (preference === "default" && operatorDefault);
