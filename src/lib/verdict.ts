/* ============================================================
 * verdict — plain-language result summary (§14.3, Batch J)
 * Turns a RunResult into a short, jargon-free human verdict so a
 * non-technical user understands the result in words, not just
 * numbers ("Great for 4K streaming, video calls, and large
 * downloads"). Derived purely from measured download/upload Mbps +
 * latency thresholds — no compensation, no units coupling (works
 * off raw bps, converted to Mbps here for the threshold compare).
 *
 * Tiers are intentionally coarse and generous; the goal is
 * confidence, not precision. A caveat line flags a latency problem
 * (high ping / bufferbloat) that the raw speed wouldn't reveal.
 * ============================================================ */

import type { RunResult } from "./runner/contract";

export type VerdictTier = "excellent" | "good" | "ok" | "basic";

export interface Verdict {
  tier: VerdictTier;
  /** One-line headline a newcomer reads first. */
  headline: string;
  /** Plain-language "good for…" detail. */
  detail: string;
  /** Optional caveat (latency/bufferbloat) — null when nothing to flag. */
  caveat: string | null;
}

/** Download speed in Mbps (the threshold currency, independent of display unit). */
function mbps(bps: number): number {
  return bps / 1e6;
}

export function deriveVerdict(result: RunResult): Verdict {
  const down = mbps(result.download?.meanBps ?? 0);
  const up = mbps(result.upload?.meanBps ?? 0);
  const ping = result.latency?.p50Ms ?? result.latency?.idleMs ?? 0;
  const grade = result.bufferbloat?.grade ?? "A";

  // ---- Speed tier (driven mostly by download, the dominant everyday signal) ----
  let tier: VerdictTier;
  if (down >= 200) tier = "excellent";
  else if (down >= 50) tier = "good";
  else if (down >= 15) tier = "ok";
  else tier = "basic";

  const HEADLINE: Record<VerdictTier, string> = {
    excellent: "Excellent connection",
    good: "Solid connection",
    ok: "Decent connection",
    basic: "Basic connection",
  };
  const DETAIL: Record<VerdictTier, string> = {
    excellent: "Great for 4K streaming, video calls, and large downloads — even on several devices at once.",
    good: "Good for HD streaming, video calls, and everyday downloads.",
    ok: "Best for HD streaming on a device or two, browsing, and music.",
    basic: "Best for browsing, email, and standard-definition video.",
  };

  // ---- Caveat: a latency/bufferbloat problem speed alone wouldn't reveal ----
  let caveat: string | null = null;
  if (grade === "D" || grade === "F") {
    caveat =
      "Heads up: the connection slows down a lot when busy, so calls and games may lag during big downloads.";
  } else if (ping >= 120) {
    caveat =
      "Heads up: ping is on the high side, which can add a noticeable lag to video calls and gaming.";
  } else if (up > 0 && up < 5 && (tier === "excellent" || tier === "good")) {
    caveat =
      "Note: upload is modest, which can matter for video calls and sending large files.";
  }

  return { tier, headline: HEADLINE[tier], detail: DETAIL[tier], caveat };
}
