// LocalStorage schema for user settings.
// Load merges stale or partial blobs onto the current defaults.
import type { PingCadence, RunnerConfig } from "../runner/contract";
import { normalizeStreamCount } from "../runner/real/streamPolicy";
import { DEFAULT_CONFIG } from "./store.svelte";

const STORAGE_VERSION = 1;
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

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

// Persisted blobs may be stale, partial, or hand-edited.
// Keys absent from the defaults and leaves of the wrong type fall back.
function deepMergeOverDefaults<T>(base: T, source: unknown): T {
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

function coercePingCadence(value: unknown, fallback: PingCadence): PingCadence {
  if (value === "instant") return "reply-driven";
  return value === "reply-driven" ||
    value === "fast" ||
    value === "medium" ||
    value === "slow"
    ? value
    : fallback;
}

export function loadPersisted(): PersistedState {
  const defaults = defaultPersisted();
  if (typeof window === "undefined") return defaults;
  let raw: string | null = null;
  try {
    raw = window.localStorage.getItem(STORAGE_KEY);
  } catch {
    // Storage access throws outright when site data is blocked.
    return defaults;
  }
  const parsed = safeParse(raw);
  if (!isPlainObject(parsed)) return defaults;
  const merged = deepMergeOverDefaults(defaults, parsed);

  // deepMergeOverDefaults walks only keys the current schema defines.
  // Legacy spellings need explicit mapping below.
  const parsedConfig = isPlainObject(parsed.config) ? parsed.config : null;
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
  const legacyTransports = isPlainObject(parsedConfig?.transports)
    ? parsedConfig.transports
    : null;
  if (typeof legacyTransports?.throughputTarget === "string")
    merged.config.transports.throughputTarget =
      legacyTransports.throughputTarget;
  else if (typeof legacyTransports?.transfer === "string")
    merged.config.transports.throughputTarget = legacyTransports.transfer;
  if (typeof legacyTransports?.latencyTarget === "string")
    merged.config.transports.latencyTarget = legacyTransports.latencyTarget;
  else if (typeof legacyTransports?.latency === "string")
    merged.config.transports.latencyTarget = legacyTransports.latency;
  const legacyEndpoint = isPlainObject(parsedConfig?.endpoint)
    ? parsedConfig.endpoint
    : null;
  const legacyProtocol = legacyEndpoint?.protocol;
  switch (legacyProtocol) {
    case "current":
    case "http2":
    case "http3":
      merged.config.transports.throughputTarget = legacyProtocol;
      break;
    case "http1":
      merged.config.transports.throughputTarget = "http1-clear";
      break;
  }
  if (typeof parsedConfig?.parallelStreams === "number")
    merged.config.transferStreams.count = normalizeStreamCount(
      parsedConfig.parallelStreams,
    );
  const parsedCompensation = isPlainObject(parsedConfig?.compensation)
    ? parsedConfig.compensation
    : null;
  const parsedParams = isPlainObject(parsedCompensation?.params)
    ? parsedCompensation.params
    : null;
  // A numeric IP family is an explicit expert override and survives hydration.
  const savedIPVersion = parsedParams?.ipVersion;
  if (savedIPVersion === "auto" || savedIPVersion === 4 || savedIPVersion === 6)
    merged.config.compensation.params.ipVersion = savedIPVersion;
  if (
    !["lan", "loopback", "tunnel", "custom"].includes(
      merged.config.compensation.profile,
    )
  )
    merged.config.compensation.profile = "lan";
  if (
    !["auto", "http1-clear", "https-tls", "http2", "http3-quic"].includes(
      merged.config.compensation.transport,
    )
  )
    merged.config.compensation.transport = "auto";
  if (
    merged.config.transports.throughputTarget === "current" ||
    /^(http[123]|http1-(clear|tls))$/.test(
      merged.config.transports.throughputTarget,
    )
  )
    merged.config.transports.throughputTarget = "auto";
  if (/^ws-http1-(clear|tls)$/.test(merged.config.transports.latencyTarget))
    merged.config.transports.latencyTarget = "auto";
  if (!["auto", "forced"].includes(merged.config.transferStreams.mode))
    merged.config.transferStreams.mode = "auto";
  merged.config.transferStreams.count = normalizeStreamCount(
    merged.config.transferStreams.count,
  );
  return merged;
}

export function savePersisted(snapshot: PersistedState): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(snapshot));
  } catch {
    // Blocked site data or a full quota must not break the session.
    // Settings then do not survive a reload.
  }
}
