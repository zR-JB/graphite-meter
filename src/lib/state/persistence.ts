/* ============================================================
 * The Graphite Meter — Persistence Layer (§14.1, Batch H)
 *
 * Versioned, merge-tolerant localStorage hydration + save for the
 * console store. Pure helpers only — the store wires the debounced
 * save $effect and calls `loadPersisted()` at construction.
 *
 * Contract:
 *   - Stored under a single VERSIONED key (`graphite-meter:v1`).
 *   - On read, the parsed blob is DEEP-MERGED over the defaults so a
 *     missing field falls back and an unknown/extra field is dropped.
 *     Corrupt JSON (or a non-object blob) → ignore, use defaults.
 *   - Everything is guarded with `typeof window !== "undefined"` so
 *     the module is import-safe in a non-browser context.
 * ============================================================ */

import type { RunnerConfig } from "../runner/contract";
import { DEFAULT_CONFIG } from "./console.svelte";

/** Bump when the persisted SHAPE changes incompatibly. The key itself
 *  carries the version so an old blob under `:v1` is simply never read by
 *  a future `:v2` build (and vice-versa) — no migration code required. */
export const STORAGE_VERSION = 1;
export const STORAGE_KEY = `graphite-meter:v${STORAGE_VERSION}`;

export type ThemePref = "dark" | "light";
export type UxMode = "simple" | "advanced";

/** The full persisted snapshot. Display prefs + the entire RunnerConfig.
 *  `uxMode` is included now (default "simple") so Batch I can consume it
 *  without reworking storage. */
export interface PersistedState {
  config: RunnerConfig;
  unitBase: "base10" | "base2";
  unitKind: "bits" | "bytes";
  theme: ThemePref;
  uxMode: UxMode;
}

/** System-preference default for theme when nothing is saved yet. */
export function systemThemeDefault(): ThemePref {
  if (typeof window === "undefined" || !window.matchMedia) return "dark";
  return window.matchMedia("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

/** The defaults the persisted blob merges OVER. Theme defaults to the
 *  system preference; everything else to the store/config defaults. */
export function defaultPersisted(): PersistedState {
  return {
    config: structuredClone(DEFAULT_CONFIG),
    unitBase: "base10",
    unitKind: "bits",
    theme: systemThemeDefault(),
    uxMode: "simple",
  };
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Recursively merge `source` OVER `base`, key-by-key, keyed off the shape of
 * `base` (the defaults). This makes the merge tolerant in both directions:
 *   - a key absent from `source` keeps the default;
 *   - a key present in `source` but NOT in `base` (an unknown/legacy field)
 *     is dropped — we only ever walk keys that exist in the defaults;
 *   - a type mismatch (e.g. saved a string where a number is expected, or an
 *     object where a scalar is expected) is rejected, keeping the default.
 * Arrays and scalars are replaced wholesale (only when the type matches).
 */
function deepMergeOverDefaults<T>(base: T, source: unknown): T {
  if (!isPlainObject(base)) {
    // Scalar / array leaf: accept the saved value only if its broad type
    // matches the default's, else keep the default.
    if (source === undefined) return base;
    if (Array.isArray(base)) return (Array.isArray(source) ? source : base) as T;
    return (typeof source === typeof base ? source : base) as T;
  }
  if (!isPlainObject(source)) return base; // saved a non-object → ignore it
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(base as Record<string, unknown>)) {
    out[key] = deepMergeOverDefaults(
      (base as Record<string, unknown>)[key],
      source[key],
    );
  }
  return out as T;
}

/** Safe JSON parse — never throws; returns `null` on any failure. */
function safeParse(raw: string | null): unknown {
  if (raw == null) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

/**
 * Hydrate the persisted state. Reads the versioned key, safe-parses, and
 * deep-merges over `defaultPersisted()`. Any failure (no window, no key,
 * corrupt JSON, wrong type) cleanly yields the defaults.
 */
export function loadPersisted(): PersistedState {
  const defaults = defaultPersisted();
  if (typeof window === "undefined") return defaults;
  let raw: string | null = null;
  try {
    raw = window.localStorage.getItem(STORAGE_KEY);
  } catch {
    return defaults; // storage disabled / blocked → defaults
  }
  const parsed = safeParse(raw);
  if (!isPlainObject(parsed)) return defaults;
  return deepMergeOverDefaults(defaults, parsed);
}

/** Serialize + write the snapshot under the versioned key. Never throws
 *  (quota / disabled storage is swallowed). */
export function savePersisted(snapshot: PersistedState): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(snapshot));
  } catch {
    /* storage full or disabled — non-fatal, settings just won't persist */
  }
}
