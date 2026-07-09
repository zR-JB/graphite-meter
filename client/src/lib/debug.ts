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

function scale(value: number, units: string[]): [string, string] {
  let v = value;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  return [v.toFixed(2), units[i]];
}
