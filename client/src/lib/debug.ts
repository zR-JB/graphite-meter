/* ============================================================
 * The Graphite Meter — Debug logging (dev diagnostics)
 * ============================================================
 *
 * A tiny, dependency-free console logger gated behind a single flag
 * (Settings › Developer › "Debug logging", persisted). When ON, the
 * runner/core/workers emit verbose, COMPONENT-TAGGED, unit-explicit
 * lines so a flood of logs stays readable — every line says WHO
 * emitted it, WHAT it measured, and in WHICH units. Pairs with the Go
 * server's `-verbose` flag (`[gm:server:download]` / `[gm:server:upload]`)
 * so client- and server-side throughput can be compared second-by-second
 * against the kernel interface counters (btop).
 *
 * Threading note: this module is imported by BOTH the main thread
 * (RealRunner, core, wire) and each stream worker. Workers are separate
 * module graphs, so each has its OWN `enabled` flag — the main thread
 * sets its own via setDebugLogging(); a worker is told the current value
 * in its `start` message and calls setDebugLogging() itself. There is no
 * shared state to keep in sync beyond that one boolean.
 *
 * Line shape:
 *   [gm:dl-worker#2] raw-receive · rate=9.31 Gbit/s  window=1.16 GB  total=8.20 GB  dt=1001 ms
 * ============================================================ */

let enabled = false;

/** Toggle verbose logging for THIS thread (main thread or one worker). */
export function setDebugLogging(on: boolean): void {
  enabled = on;
}

/** Whether verbose logging is on for this thread — cheap guard for hot paths. */
export function debugEnabled(): boolean {
  return enabled;
}

/**
 * Emit one tagged line. `component` is the emitter (e.g. `"dl-worker#2"`,
 * `"realrunner:aggregate"`, `"core:throughput"`); `event` is what happened;
 * `fields` are already-formatted key=value pairs (use the fmt* helpers so the
 * units travel with the value). No-op unless logging is enabled.
 */
export function dlog(
  component: string,
  event: string,
  fields?: Record<string, string | number>,
): void {
  if (!enabled) return;
  let line = `[gm:${component}] ${event}`;
  if (fields) {
    const parts = Object.entries(fields).map(([k, v]) => `${k}=${v}`);
    if (parts.length) line += " · " + parts.join("  ");
  }
  console.log(line);
}

/* ---------- unit-explicit formatters (SI base-10, the project default) ----------
 * Self-describing so a log line is unambiguous at any speed — dial-up reads in
 * kbit/s, a localhost test in Gbit/s, from the same call site. */

/** Bytes/sec → an auto-scaled SI bit-rate string, e.g. "9.31 Gbit/s". */
export function fmtRate(bytesPerSec: number): string {
  const [v, u] = scale(bytesPerSec * 8, ["bit/s", "kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"]);
  return `${v} ${u}`;
}

/** A byte count → an auto-scaled SI string, e.g. "1.16 GB". */
export function fmtBytes(n: number): string {
  const [v, u] = scale(n, ["B", "kB", "MB", "GB", "TB"]);
  return `${v} ${u}`;
}

/** Milliseconds → a fixed "1001 ms" string (whole ms; sub-ms is noise here). */
export function fmtMs(ms: number): string {
  return `${ms.toFixed(0)} ms`;
}

/** Step `value` up the 1000-spaced `units` ladder; returns [fixed-2 value, unit]. */
function scale(value: number, units: string[]): [string, string] {
  let v = value;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  return [v.toFixed(2), units[i]];
}
