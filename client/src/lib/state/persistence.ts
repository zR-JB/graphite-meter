// LocalStorage schema for user settings. Load is defensive so old or partial
// blobs merge onto the current defaults instead of breaking startup.
import type { RunnerConfig } from "../runner/contract";
import { DEFAULT_CONFIG } from "./store.svelte";

export const STORAGE_VERSION = 1;
export const STORAGE_KEY = `graphite-meter:v${STORAGE_VERSION}`;

export type ThemePref = "dark" | "light" | "auto";

export const DEFAULT_DOCK_WIDTH = { left: 400, right: 400 };

export type SettingsTab = "setup" | "developer";

export interface PersistedState {
  config: RunnerConfig;
  unitBase: "base10" | "base2";
  unitKind: "bits" | "bytes";
  theme: ThemePref;
  showWireEstimates: boolean;
  dockWidth: { left: number; right: number };
  settingsTab: SettingsTab;
  debugLogging: boolean;
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
    showWireEstimates: false,
    dockWidth: { ...DEFAULT_DOCK_WIDTH },
    settingsTab: "setup",
    debugLogging: false,
  };
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function deepMergeOverDefaults<T>(base: T, source: unknown): T {
  // Persisted blobs may be old, partial, or hand-edited. Walk only default keys
  // and type-check leaves so schema changes fall back instead of crashing load.
  if (!isPlainObject(base)) {
    if (source === undefined) return base;
    if (Array.isArray(base))
      return (Array.isArray(source) ? source : base) as T;
    return (typeof source === typeof base ? source : base) as T;
  }
  if (!isPlainObject(source)) return base;
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(base as Record<string, unknown>)) {
    out[key] = deepMergeOverDefaults(
      (base as Record<string, unknown>)[key],
      source[key],
    );
  }
  return out as T;
}

function safeParse(raw: string | null): unknown {
  if (raw == null) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
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
  return deepMergeOverDefaults(defaults, parsed);
}

export function savePersisted(snapshot: PersistedState): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(snapshot));
  } catch {}
}
