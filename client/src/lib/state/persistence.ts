import type { PingCadence, RunnerConfig } from "../runner/contract";
import { normalizeStreamCount } from "../runner/real/streamPolicy";
import { canonicalAdaptiveConfig, DEFAULT_CONFIG } from "./defaults";

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

export interface PersistedState {
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

function safeParse(raw: string | null): unknown {
  if (raw == null) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function coercePingCadence(value: unknown, fallback: PingCadence): PingCadence {
  if (value === "instant") return "reply-driven";
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

function firstString(...values: unknown[]): string | undefined {
  return values.find((value): value is string => typeof value === "string");
}

export function loadPersisted(): PersistedState {
  const defaults = defaultPersisted();
  if (typeof window === "undefined") return defaults;
  let raw: string | null = null;
  try {
    raw = window.localStorage.getItem(STORAGE_KEY);
  } catch {
    return defaults;
  }
  const parsed = safeParse(raw);
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
  const legacyPingConcurrency = parsedConfig?.pingConcurrency;
  if (legacyPingConcurrency !== undefined)
    merged.config.pingCadence = coercePingCadence(
      legacyPingConcurrency,
      merged.config.pingCadence,
    );
  const legacyTransports = object(parsedConfig?.transports);
  const throughputTarget = firstString(
    legacyTransports?.throughputTarget,
    legacyTransports?.transfer,
  );
  const latencyTarget = firstString(
    legacyTransports?.latencyTarget,
    legacyTransports?.latency,
  );
  if (throughputTarget !== undefined)
    merged.config.transports.throughputTarget = throughputTarget;
  if (latencyTarget !== undefined)
    merged.config.transports.latencyTarget = latencyTarget;
  if (typeof parsedConfig?.parallelStreams === "number")
    merged.config.transferStreams.count = normalizeStreamCount(
      parsedConfig.parallelStreams,
    );
  if (
    merged.config.transports.throughputTarget === "current" ||
    /^(http[123]|http1-(clear|tls))$/.test(
      merged.config.transports.throughputTarget,
    )
  )
    merged.config.transports.throughputTarget = "auto";
  if (/^ws-http1-(clear|tls)$/.test(merged.config.transports.latencyTarget))
    merged.config.transports.latencyTarget = "auto";
  if (!oneOf(merged.config.transferStreams.mode, ["auto", "forced"]))
    merged.config.transferStreams.mode = "auto";
  merged.config.transferStreams.count = normalizeStreamCount(
    merged.config.transferStreams.count,
  );
  return merged;
}

export function savePersisted(snapshot: PersistedState): void {
  if (typeof window === "undefined") return;
  try {
    const safe = structuredClone(snapshot);
    const adaptive = canonicalAdaptiveConfig(snapshot.config.adaptive);
    const serialized = {
      ...safe,
      config: {
        ...safe.config,
        adaptive: { enabled: adaptive.enabled },
      },
    };
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(serialized));
  } catch {}
}
