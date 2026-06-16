/* ============================================================
 * The Graphite Meter — Format Utilities (§10)
 * Guarantee the "zero layout shift" mandate. All numeric
 * renders pass through here.
 * ============================================================ */

/** Convert raw bps → display value in active unit (mirrors store.toUnit). */
export function toDisplaySpeed(
  bps: number,
  base: "base10" | "base2",
  kind: "bits" | "bytes",
): number {
  const div = base === "base10" ? 1e6 : 2 ** 20;
  return kind === "bits" ? bps / div : bps / 8 / div;
}

/** Fixed-width formatting: always returns same char count for a magnitude band
 *  so tabular-nums + this guarantee zero reflow. */
export function fmtSpeed(v: number): string {
  if (v >= 1000) return v.toFixed(0); // 1000+ → integer
  if (v >= 100) return v.toFixed(1); // 100.0
  return v.toFixed(2); // 12.34
}

export function fmtMs(ms: number): string {
  return ms < 100 ? ms.toFixed(1) : ms.toFixed(0);
}

export function fmtBytes(b: number, base: "base10" | "base2"): string {
  const k = base === "base10" ? 1000 : 1024;
  const u = base === "base10" ? ["B", "KB", "MB", "GB"] : ["B", "KiB", "MiB", "GiB"];
  let i = 0;
  let n = b;
  while (n >= k && i < u.length - 1) {
    n /= k;
    i++;
  }
  return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
}

/** "Nice" axis ceiling for charts. */
export function niceCeil(v: number): number {
  if (v <= 0) return 1;
  const exp = Math.floor(Math.log10(v));
  const f = v / 10 ** exp;
  const nf = f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10;
  return nf * 10 ** exp;
}

/** Count-up tween generator for result snap (rAF-driven, 220ms ease-out). */
export function countUp(
  from: number,
  to: number,
  durMs: number,
  onTick: (v: number) => void,
): () => void {
  const start = performance.now();
  let raf = 0;
  const ease = (t: number) => 1 - Math.pow(1 - t, 3); // matches --ease-out feel
  const loop = (now: number) => {
    const t = Math.min(1, (now - start) / durMs);
    onTick(from + (to - from) * ease(t));
    if (t < 1) raf = requestAnimationFrame(loop);
  };
  raf = requestAnimationFrame(loop);
  return () => cancelAnimationFrame(raf);
}
