import { isRecord } from "../api/decode";
import type { RunnerConfig } from "../runner/contract";
import { normalizeStreamCount } from "../runner/paths";
import { clampDuration, DEFAULT_CONFIG, DURATION_LIMITS } from "./defaults";

type DurationKey = keyof RunnerConfig["duration"];

const STORAGE_VERSION = 1;
export const STORAGE_KEY = `graphite-meter:v${STORAGE_VERSION}`;

export type ThemePref = "dark" | "light" | "auto";
export type ResultHistoryPreference = "default" | "enabled" | "disabled";

export function resolveResultHistoryPreference(
  preference: ResultHistoryPreference,
  operatorDefault: boolean,
): boolean {
  return (
    preference === "enabled" || (preference === "default" && operatorDefault)
  );
}
export type HistoryColumn =
  "download" | "upload" | "bidirectional" | "idle" | "loaded";

export const HISTORY_COLUMNS: readonly HistoryColumn[] = [
  "download",
  "upload",
  "bidirectional",
  "idle",
  "loaded",
];
export const DEFAULT_HISTORY_COLUMNS: readonly HistoryColumn[] = [
  "download",
  "upload",
  "idle",
  "loaded",
];

export const DEFAULT_DOCK_WIDTH = { left: 420, right: 420 };
/** History's list takes this share of the width beside a result. */
export const DEFAULT_HISTORY_SPLIT = 0.5;

interface PersistedState {
  config: RunnerConfig;
  unitBase: "base10" | "base2";
  unitKind: "bits" | "bytes";
  theme: ThemePref;
  showWireEstimates: boolean;
  keyShortcuts: boolean;
  resultHistoryPreference: ResultHistoryPreference;
  historyColumns: HistoryColumn[];
  dockWidth: { left: number; right: number };
  historySplit: number;
}

export function systemThemeDefault(): "dark" | "light" {
  if (typeof window === "undefined" || !window.matchMedia) return "dark";
  return window.matchMedia("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

export function defaultPersisted(): PersistedState {
  return {
    config: structuredClone(DEFAULT_CONFIG),
    unitBase: "base10",
    unitKind: "bits",
    theme: "auto",
    showWireEstimates: true,
    keyShortcuts: true,
    resultHistoryPreference: "default",
    historyColumns: [...DEFAULT_HISTORY_COLUMNS],
    dockWidth: { ...DEFAULT_DOCK_WIDTH },
    historySplit: DEFAULT_HISTORY_SPLIT,
  };
}

/** Storage can be absent, blocked or full; a preference then lasts for this page only. */
export function readStored(key: string): unknown {
  try {
    return JSON.parse(window.localStorage.getItem(key) ?? "null");
  } catch {
    return null;
  }
}

export function writeStored(key: string, value: unknown): boolean {
  try {
    window.localStorage.setItem(key, JSON.stringify(value));
    return true;
  } catch {
    return false;
  }
}

const record = (value: unknown): Record<string, unknown> =>
  isRecord(value) ? value : {};
const choice = <T extends string>(
  value: unknown,
  values: readonly T[],
  fallback: T,
): T =>
  typeof value === "string" && values.includes(value as T)
    ? (value as T)
    : fallback;
const flag = (value: unknown, fallback: boolean) =>
  typeof value === "boolean" ? value : fallback;
const text = (value: unknown, fallback: string) =>
  typeof value === "string" && value.length <= 256 ? value : fallback;
const positive = <T>(value: unknown, fallback: T): number | T =>
  typeof value === "number" && Number.isFinite(value) && value >= 1
    ? value
    : fallback;
const share = (value: unknown, fallback: number) =>
  typeof value === "number" && value >= 0 && value <= 1 ? value : fallback;
const CADENCES = ["reply-driven", "fast", "medium", "slow"] as const;

export function loadPersisted(): PersistedState {
  const defaults = defaultPersisted();
  const base = defaults.config;
  const saved = record(readStored(STORAGE_KEY));
  const config = record(saved.config);
  const stages = record(config.stages);
  const duration = record(config.duration);
  // Earlier versions skipped a stage by giving it 0 ms; only its flag skips now.
  const skipped = (key: DurationKey) =>
    key !== "warmupMs" &&
    typeof duration[key] === "number" &&
    duration[key] <= 0;
  const streams = record(config.transferStreams);
  const transports = record(config.transports);
  const dock = record(saved.dockWidth);
  const gaugeMax = record(config.visualization).throughputMaxBytesPerSec;
  const columns = Array.isArray(saved.historyColumns)
    ? [
        ...new Set(
          saved.historyColumns.filter((column): column is HistoryColumn =>
            HISTORY_COLUMNS.includes(column),
          ),
        ),
      ]
    : [];
  return {
    config: {
      stages: {
        latency:
          flag(stages.latency, base.stages.latency) && !skipped("latencyMs"),
        download:
          flag(stages.download, base.stages.download) && !skipped("downloadMs"),
        upload: flag(stages.upload, base.stages.upload) && !skipped("uploadMs"),
        bidirectional:
          flag(stages.bidirectional, base.stages.bidirectional) &&
          !skipped("bidirectionalMs"),
      },
      skipLoadedLatencyWhenStageOff: flag(
        config.skipLoadedLatencyWhenStageOff,
        base.skipLoadedLatencyWhenStageOff,
      ),
      duration: Object.fromEntries(
        (Object.keys(DURATION_LIMITS) as DurationKey[]).map((key) => [
          key,
          clampDuration(key, skipped(key) ? undefined : duration[key]),
        ]),
      ) as RunnerConfig["duration"],
      pingCadence: choice(config.pingCadence, CADENCES, base.pingCadence),
      loadedPingCadence: choice(
        config.loadedPingCadence,
        CADENCES,
        base.loadedPingCadence,
      ),
      transferStreams: {
        mode: choice(streams.mode, ["auto", "forced"], "auto"),
        count:
          typeof streams.count === "number"
            ? normalizeStreamCount(streams.count)
            : base.transferStreams.count,
      },
      experimentalDatagramThroughput: flag(
        config.experimentalDatagramThroughput,
        base.experimentalDatagramThroughput,
      ),
      transports: {
        throughputTarget: text(
          transports.throughputTarget,
          base.transports.throughputTarget,
        ),
        latencyTarget: text(
          transports.latencyTarget,
          base.transports.latencyTarget,
        ),
      },
      // Earlier versions saved { enabled }.
      adaptive: flag(
        record(config.adaptive).enabled ?? config.adaptive,
        base.adaptive,
      ),
      visualization: {
        throughputMaxBytesPerSec: positive(gaugeMax, "auto" as const),
      },
    },
    unitBase: choice(saved.unitBase, ["base10", "base2"], defaults.unitBase),
    unitKind: choice(saved.unitKind, ["bits", "bytes"], defaults.unitKind),
    theme: choice(saved.theme, ["dark", "light", "auto"], defaults.theme),
    showWireEstimates: flag(saved.showWireEstimates, true),
    keyShortcuts: flag(saved.keyShortcuts, true),
    resultHistoryPreference: choice(
      saved.resultHistoryPreference,
      ["default", "enabled", "disabled"],
      "default",
    ),
    historyColumns: columns.length ? columns : [...DEFAULT_HISTORY_COLUMNS],
    dockWidth: {
      left: positive(dock.left, DEFAULT_DOCK_WIDTH.left),
      right: positive(dock.right, DEFAULT_DOCK_WIDTH.right),
    },
    historySplit: share(saved.historySplit, DEFAULT_HISTORY_SPLIT),
  };
}

export const savePersisted = (snapshot: PersistedState): boolean =>
  writeStored(STORAGE_KEY, snapshot);
