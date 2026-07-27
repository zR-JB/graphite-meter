// Tiny per-thread logger. Workers have their own module instance, so the main
// thread passes the current flag in each worker start message.
let enabled = false;

export function setDebugLogging(on: boolean): void {
  enabled = on;
}

export function debugEnabled(): boolean {
  return enabled;
}

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

// Unit-explicit formatters keep high-volume runner logs comparable with server
// logs and external counters.
export function fmtRate(bytesPerSec: number): string {
  const [v, u] = scale(bytesPerSec * 8, [
    "bit/s",
    "kbit/s",
    "Mbit/s",
    "Gbit/s",
    "Tbit/s",
  ]);
  return `${v} ${u}`;
}

export function fmtBytes(n: number): string {
  const [v, u] = scale(n, ["B", "kB", "MB", "GB", "TB"]);
  return `${v} ${u}`;
}

export function fmtMs(ms: number): string {
  return `${ms.toFixed(0)} ms`;
}

/** A 1 Hz window over a byte counter, for the verbose per-lane logs. `add`
 *  returns the window's formatted fields once a second has passed and null
 *  until then, so a caller only formats what it logs. */
export class DebugWindow {
  #windowBytes = 0;
  #total = 0;
  #startedAt = 0;

  reset(now = performance.now()): void {
    this.#windowBytes = 0;
    this.#total = 0;
    this.#startedAt = now;
  }

  add(
    bytes: number,
    now = performance.now(),
  ): { rate: string; window: string; total: string; dt: string } | null {
    this.#windowBytes += bytes;
    this.#total += bytes;
    const dt = now - this.#startedAt;
    if (dt < 1000) return null;
    const fields = {
      rate: fmtRate(this.#windowBytes / (dt / 1000)),
      window: fmtBytes(this.#windowBytes),
      total: fmtBytes(this.#total),
      dt: fmtMs(dt),
    };
    this.#windowBytes = 0;
    this.#startedAt = now;
    return fields;
  }
}

function scale(value: number, units: string[]): [string, string] {
  let v = value;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  return [v.toFixed(2), units[i]];
}
