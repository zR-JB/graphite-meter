import type { PingCadence, RunnerConfig } from "../runner/contract";
import { normalizeStreamCount } from "../runner/paths";
import {
  canonicalAdaptiveConfig,
  clampDuration,
  DEFAULT_CONFIG,
  DURATION_LIMITS,
} from "./defaults";

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

export const DEFAULT_DOCK_WIDTH = { left: 400, right: 400 };

export interface LatencySelection {
  mode: "primary" | "all";
  serverId: string;
}

interface PersistedState {
  latencySelection: LatencySelection;
  config: RunnerConfig;
  unitBase: "base10" | "base2";
  unitKind: "bits" | "bytes";
  theme: ThemePref;
  showWireEstimates: boolean;
  resultHistoryPreference: ResultHistoryPreference;
  historyColumns: HistoryColumn[];
  dockWidth: { left: number; right: number };
}

export function systemThemeDefault(): "dark" | "light" {
  if (typeof window === "undefined" || !window.matchMedia) return "dark";
  return window.matchMedia("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

export function defaultPersisted(): PersistedState {
  return {
    latencySelection: { mode: "primary", serverId: "" },
    config: structuredClone(DEFAULT_CONFIG),
    unitBase: "base10",
    unitKind: "bits",
    theme: "auto",
    showWireEstimates: true,
    resultHistoryPreference: "default",
    historyColumns: [...DEFAULT_HISTORY_COLUMNS],
    dockWidth: { ...DEFAULT_DOCK_WIDTH },
  };
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function deepMergeOverDefaults<T>(base: T, source: unknown): T {
  if (!isPlainObject(base)) {
    if (Array.isArray(base))
      return (Array.isArray(source) ? source : base) as T;
    return source !== undefined && typeof source === typeof base
      ? (source as T)
      : base;
  }
  if (!isPlainObject(source)) return base;
  return Object.fromEntries(
    Object.keys(base).map((key) => [
      key,
      deepMergeOverDefaults(base[key], source[key]),
    ]),
  ) as T;
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

function coercePingCadence(value: unknown, fallback: PingCadence): PingCadence {
  return oneOf(value, ["reply-driven", "fast", "medium", "slow"])
    ? value
    : fallback;
}

function object(value: unknown): Record<string, unknown> | null {
  return isPlainObject(value) ? value : null;
}

function oneOf<T extends string>(
  value: unknown,
  values: readonly T[],
): value is T {
  return typeof value === "string" && values.includes(value as T);
}

export function loadPersisted(): PersistedState {
  const defaults = defaultPersisted();
  const parsed = readStored(STORAGE_KEY);
  if (!isPlainObject(parsed)) return defaults;
  const merged = deepMergeOverDefaults(defaults, parsed);
  if (
    !oneOf(parsed.resultHistoryPreference, ["default", "enabled", "disabled"])
  )
    merged.resultHistoryPreference = "default";
  const historyColumns = Array.isArray(parsed.historyColumns)
    ? [
        ...new Set(
          parsed.historyColumns.filter((column): column is HistoryColumn =>
            oneOf(column, HISTORY_COLUMNS),
          ),
        ),
      ]
    : [];
  merged.historyColumns = historyColumns.length
    ? historyColumns
    : [...DEFAULT_HISTORY_COLUMNS];

  if (!oneOf(merged.latencySelection.mode, ["primary", "all"]))
    merged.latencySelection.mode = "primary";
  if (
    typeof merged.latencySelection.serverId !== "string" ||
    merged.latencySelection.serverId.length > 128
  )
    merged.latencySelection.serverId = "";

  const parsedConfig = object(parsed.config);
  const parsedAdaptive = object(parsedConfig?.adaptive);
  // Adaptive tuning is internal policy; preserve only its enable preference.
  merged.config.adaptive = canonicalAdaptiveConfig(parsedAdaptive);
  merged.config.pingCadence = coercePingCadence(
    parsedConfig?.pingCadence,
    defaults.config.pingCadence,
  );
  merged.config.loadedPingCadence = coercePingCadence(
    parsedConfig?.loadedPingCadence,
    defaults.config.loadedPingCadence,
  );
  if (!oneOf(merged.config.transferStreams.mode, ["auto", "forced"]))
    merged.config.transferStreams.mode = "auto";
  merged.config.transferStreams.count = normalizeStreamCount(
    merged.config.transferStreams.count,
  );
  for (const key of Object.keys(DURATION_LIMITS) as DurationKey[])
    merged.config.duration[key] = clampDuration(
      key,
      merged.config.duration[key],
    );
  return merged;
}

export function savePersisted(snapshot: PersistedState): boolean {
  const { enabled } = canonicalAdaptiveConfig(snapshot.config.adaptive);
  return writeStored(STORAGE_KEY, {
    ...snapshot,
    config: { ...snapshot.config, adaptive: { enabled } },
  });
}
