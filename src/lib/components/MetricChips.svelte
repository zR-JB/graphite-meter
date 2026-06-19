<script lang="ts">
  /* ============================================================
   * <MetricChips> — live measured-vs-estimated cards (§13.3)
   * Replaces the Download/Upload/Ping placeholder grid in the
   * center stage. Per metric it shows the MEASURED browser value
   * and, beneath it, the COMPENSATED wire-rate estimate. A small
   * stability pip (low/medium/high) — sourced from the runner's live
   * `liveStability` / the result's `band`, NOT the overhead estimate —
   * climbs as the connection settles. Each visible stage becomes a card via
   * one shared {#snippet} fed a typed CardVM (download, upload, bidirectional,
   * ping). Ping has no wire-rate compensation; bidirectional shows a combined
   * headline with a per-direction subline and no live pip.
   *
   * Reactivity (§13.3): the live cards read the store's O(1)
   * `liveCompensation` derived (protocol/config multipliers on the
   * latest bytesPerSec); the post-run cards read `downloadCompensation` /
   * `uploadCompensation`, which recompute only when `result`
   * changes. This component never iterates sample arrays.
   *
   * Zero layout shift: tabular-nums + fixed-band fmtSpeed, fixed
   * pip width, and a fixed two-line value block so the estimate
   * line is always reserved even when absent.
   * ============================================================ */
  import { untrack } from "svelte";
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs, countUp } from "../format";
  import { ICON } from "../constants";
  import { tooltip, JARGON } from "../actions/tooltip";

  const dash = "—";

  // Honour the user's motion preference once (the count-up tween is decorative).
  const reduced =
    typeof window !== "undefined" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  /** Build a transfer card's model (download or upload). Live → the store's O(1)
   *  `liveCompensation` on the latest sample + the runner's `liveStability`;
   *  resolved → the per-stage result + its phase-specific compensation. One
   *  helper for both directions — the only difference is which result + which
   *  memoized compensation derived to read. */
  function transferModel(phase: "download" | "upload") {
    const st = store.liveStability[phase];
    if (store.phase === phase) {
      const comp = store.liveCompensation;
      return {
        measuredBytesPerSec: comp.measuredBytesPerSec,
        estimatedBytesPerSec: comp.estimatedBytesPerSec,
        multiplier: comp.totalMultiplier,
        band: st?.band ?? "low",
        score: st?.score ?? 0,
        active: true,
        has: comp.measuredBytesPerSec > 0,
      };
    }
    const res = store.stageResults[phase];
    const comp = phase === "download" ? store.downloadCompensation : store.uploadCompensation;
    return {
      measuredBytesPerSec: res?.reportedBytesPerSec ?? 0,
      estimatedBytesPerSec: comp.estimatedBytesPerSec,
      multiplier: comp.totalMultiplier,
      band: res?.band ?? st?.band ?? "low",
      score: res?.stabilityScore ?? st?.score ?? 0,
      active: false,
      has: !!res,
    };
  }

  const dl = $derived.by(() => transferModel("download"));
  const ul = $derived.by(() => transferModel("upload"));

  // ---- Bidirectional card (combined down+up; live lanes → final pair) ----
  // No wire-rate estimate or live stability pip (the engine emits none for this
  // phase); it carries a per-direction subline and, when resolved, the band of
  // its down lane. Live = store.liveBidirectional; resolved = result.bidirectional.
  const bidi = $derived.by(() => {
    if (store.phase === "bidirectional") {
      const b = store.liveBidirectional ?? { down: 0, up: 0 };
      return {
        down: b.down,
        up: b.up,
        combined: b.down + b.up,
        band: "low" as const,
        score: 0,
        active: true,
        has: b.down + b.up > 0,
      };
    }
    const r = store.result?.bidirectional;
    const down = r?.down.reportedBytesPerSec ?? 0;
    const up = r?.up.reportedBytesPerSec ?? 0;
    return {
      down,
      up,
      combined: down + up,
      band: r?.down.band ?? "low",
      score: r?.down.stabilityScore ?? 0,
      active: false,
      has: !!r,
    };
  });

  // ---- Ping card (no wire-rate compensation, but it does have stability) ----
  const ping = $derived.by(() => {
    const st = store.liveStability.latency;
    if (store.phase === "latency") {
      return {
        ms: store.liveRtt,
        band: st?.band ?? "low",
        score: st?.score ?? 0,
        active: true,
        has: store.liveRtt > 0,
      };
    }
    const lat = store.stageResults.latency;
    const reported = lat?.reportedMs ?? null;
    return {
      ms: reported ?? store.liveRtt,
      band: lat?.band ?? st?.band ?? "low",
      score: lat?.stabilityScore ?? st?.score ?? 0,
      active: false,
      has: reported != null,
    };
  });

  /** Whether compensation actually lifts the value (multiplier > ~1). */
  function lifted(multiplier: number): boolean {
    return multiplier > 1.0005;
  }

  function pctLift(multiplier: number): string {
    return `+${((multiplier - 1) * 100).toFixed(1)}%`;
  }

  /* ---- Count-up snaps on complete (§13.7) ----
     Each card's MEASURED value tweens from its live reading to the final
     aggregate when a run resolves (220ms ease-out via the shared `countUp`).
     The override holds the in-flight number; while it's null the templates
     fall back to the derived live value, so during a run nothing is intercepted.
     Reduced motion skips the tween (the override is set instantly). */
  const TWEEN_MS = 220;
  let dlSnap = $state<number | null>(null);
  let ulSnap = $state<number | null>(null);
  let pingSnap = $state<number | null>(null);
  let bidiSnap = $state<number | null>(null);
  let dlCancel: (() => void) | null = null;
  let ulCancel: (() => void) | null = null;
  let pingCancel: (() => void) | null = null;
  let bidiCancel: (() => void) | null = null;

  /* ---- Progressive reveal — "reveal & keep" ----
     A card appears when its stage runs and STAYS once finished; not-yet-run
     stages are hidden until they start (final results show every enabled card).
     Since `store.result` only lands at completion, each stage's last live value
     is frozen at hand-off (the $effect below tracks the live phase every tick,
     then stops when the phase moves on — leaving the last value put) so a
     finished card keeps a real number through the later stages. */
  let pingFrozen = $state<number | null>(null);
  let dlFrozen = $state<number | null>(null);
  let ulFrozen = $state<number | null>(null);
  let bidiFrozen = $state<number | null>(null);

  function tweenTo(
    from: number,
    to: number,
    set: (v: number) => void,
  ): () => void {
    if (reduced || from === to) {
      set(to);
      return () => {};
    }
    set(from);
    return countUp(from, to, TWEEN_MS, set);
  }

  /* Per-stage count-up: each card tweens from its frozen live value to its
     FINAL result the instant that stage's result lands (not waiting for the
     whole run). One effect per stage, tracking only its own `stageResults`
     field; the "from" values are read untracked so live ticks don't retrigger.
     Stages are independent — download resolves while upload still runs. */
  $effect(() => {
    const r = store.stageResults.download;
    dlCancel?.();
    dlCancel = null;
    if (!r) {
      dlSnap = null;
      return;
    }
    dlCancel = untrack(() =>
      tweenTo(dlFrozen ?? store.toUnit(dl.measuredBytesPerSec), store.toUnit(r.reportedBytesPerSec), (v) => (dlSnap = v)),
    );
    return () => {
      dlCancel?.();
      dlCancel = null;
    };
  });

  $effect(() => {
    const r = store.stageResults.upload;
    ulCancel?.();
    ulCancel = null;
    if (!r) {
      ulSnap = null;
      return;
    }
    ulCancel = untrack(() =>
      tweenTo(ulFrozen ?? store.toUnit(ul.measuredBytesPerSec), store.toUnit(r.reportedBytesPerSec), (v) => (ulSnap = v)),
    );
    return () => {
      ulCancel?.();
      ulCancel = null;
    };
  });

  $effect(() => {
    const r = store.stageResults.latency;
    pingCancel?.();
    pingCancel = null;
    if (!r) {
      pingSnap = null;
      return;
    }
    pingCancel = untrack(() => tweenTo(pingFrozen ?? ping.ms, r.reportedMs, (v) => (pingSnap = v)));
    return () => {
      pingCancel?.();
      pingCancel = null;
    };
  });

  // Bidirectional resolves only at completion (it has no per-stage event), so its
  // count-up tweens the COMBINED rate from the frozen live value to the final sum.
  $effect(() => {
    const r = store.result?.bidirectional;
    bidiCancel?.();
    bidiCancel = null;
    if (!r) {
      bidiSnap = null;
      return;
    }
    const finalCombined = store.toUnit(r.down.reportedBytesPerSec + r.up.reportedBytesPerSec);
    bidiCancel = untrack(() =>
      tweenTo(bidiFrozen ?? store.toUnit(bidi.combined), finalCombined, (v) => (bidiSnap = v)),
    );
    return () => {
      bidiCancel?.();
      bidiCancel = null;
    };
  });

  // Freeze each stage's last live value at hand-off. While a stage is the live
  // phase this re-runs every tick (it reads that phase's live value), tracking
  // it; once the phase moves on, the branch stops firing and the value stays.
  // A fresh run (idle/warmup) clears everything so nothing leaks in early.
  $effect(() => {
    const p = store.phase;
    if (p === "idle" || p === "warmup") {
      pingFrozen = null;
      dlFrozen = null;
      ulFrozen = null;
      bidiFrozen = null;
      return;
    }
    if (p === "latency") pingFrozen = ping.ms;
    else if (p === "download") dlFrozen = store.toUnit(dl.measuredBytesPerSec);
    else if (p === "upload") ulFrozen = store.toUnit(ul.measuredBytesPerSec);
    else if (p === "bidirectional") bidiFrozen = store.toUnit(bidi.combined);
  });

  // Visibility (reveal & keep): a card shows once its enabled stage is live,
  // has been frozen (finished), or has a final result. Disabled stages never show.
  const pingShow = $derived(
    store.config.stages.latency && (ping.active || pingFrozen != null || ping.has),
  );
  const dlShow = $derived(
    store.config.stages.download && (dl.active || dlFrozen != null || dl.has),
  );
  const ulShow = $derived(
    store.config.stages.upload && (ul.active || ulFrozen != null || ul.has),
  );
  const bidiShow = $derived(
    store.config.stages.bidirectional && (bidi.active || bidiFrozen != null || bidi.has),
  );

  // Does the card have a real number to print (vs a dash)? While live, dash
  // until the first sample; once frozen/resolved, the kept/final value.
  const pingHasVal = $derived(ping.active ? ping.has : ping.has || pingFrozen != null);
  const dlHasVal = $derived(dl.active ? dl.has : dl.has || dlFrozen != null);
  const ulHasVal = $derived(ul.active ? ul.has : ul.has || ulFrozen != null);
  const bidiHasVal = $derived(bidi.active ? bidi.has : bidi.has || bidiFrozen != null);

  // The number each card actually renders: the tweened snap when present, else
  // the live value while active, else the frozen (kept) value for a finished stage.
  const dlShown = $derived(
    dlSnap ?? (dl.active ? store.toUnit(dl.measuredBytesPerSec) : dlFrozen ?? store.toUnit(dl.measuredBytesPerSec)),
  );
  const ulShown = $derived(
    ulSnap ?? (ul.active ? store.toUnit(ul.measuredBytesPerSec) : ulFrozen ?? store.toUnit(ul.measuredBytesPerSec)),
  );
  const pingShown = $derived(pingSnap ?? (ping.active ? ping.ms : pingFrozen ?? ping.ms));
  const bidiShown = $derived(
    bidiSnap ?? (bidi.active ? store.toUnit(bidi.combined) : bidiFrozen ?? store.toUnit(bidi.combined)),
  );

  /* ---- Progressive disclosure of the wire-rate estimate (§14.2) ----
     The headline numbers are always the plainly MEASURED values. The
     compensated wire-rate is a refinement, surfaced on the result cards only
     when the user opts in via the persisted Settings switch. */
  const showWire = $derived(store.showWireEstimates);

  /* ---- Card view-models ----
     One typed descriptor per visible card, consumed by the shared {#snippet}.
     Replaces four near-identical markup blocks with one list + one template. */
  type CardWire =
    | { kind: "lift"; num: string; pct: string }
    | { kind: "flat"; text: string }
    | null;
  interface CardVM {
    key: string;
    icon: string;
    ico: string; // accent class: dl | ul | bd | pg
    label: string;
    term: boolean; // dotted-underline jargon affordance (ping)
    active: boolean;
    hasVal: boolean;
    showPip: boolean;
    band: "low" | "medium" | "high";
    score: number;
    num: string; // pre-formatted, or the dash
    unit: string;
    sub?: string; // per-direction detail (bidirectional only)
    wire: CardWire;
  }

  /** Wire-rate estimate line for a transfer card (null when the user hasn't
   *  opted in). Mirrors the old inline lift/flat branch. */
  function wireFor(m: { has: boolean; multiplier: number; estimatedBytesPerSec: number }): CardWire {
    if (!showWire) return null;
    if (m.has && lifted(m.multiplier))
      return { kind: "lift", num: fmtSpeed(store.toUnit(m.estimatedBytesPerSec)), pct: pctLift(m.multiplier) };
    return { kind: "flat", text: m.has ? "no overhead applied" : "" };
  }

  const cards = $derived.by<CardVM[]>(() => {
    const out: CardVM[] = [];
    if (dlShow)
      out.push({
        key: "download", icon: ICON.download, ico: "dl", label: "Download", term: false,
        active: dl.active, hasVal: dlHasVal, showPip: dlHasVal, band: dl.band, score: dl.score,
        num: dlHasVal ? fmtSpeed(dlShown) : dash, unit: store.unitLabel, wire: wireFor(dl),
      });
    if (ulShow)
      out.push({
        key: "upload", icon: ICON.upload, ico: "ul", label: "Upload", term: false,
        active: ul.active, hasVal: ulHasVal, showPip: ulHasVal, band: ul.band, score: ul.score,
        num: ulHasVal ? fmtSpeed(ulShown) : dash, unit: store.unitLabel, wire: wireFor(ul),
      });
    if (bidiShow)
      out.push({
        key: "bidirectional", icon: ICON.bidirectional, ico: "bd", label: "Bi-dir", term: false,
        active: bidi.active, hasVal: bidiHasVal,
        // No live stability for bidi — show the pip only once resolved.
        showPip: bidiHasVal && !bidi.active, band: bidi.band, score: bidi.score,
        num: bidiHasVal ? fmtSpeed(bidiShown) : dash, unit: store.unitLabel,
        sub: bidiHasVal
          ? `↓ ${fmtSpeed(store.toUnit(bidi.down))}  ↑ ${fmtSpeed(store.toUnit(bidi.up))} ${store.unitLabel}`
          : undefined,
        wire: null,
      });
    if (pingShow)
      out.push({
        key: "latency", icon: ICON.ping, ico: "pg", label: "Ping", term: true,
        active: ping.active, hasVal: pingHasVal, showPip: pingHasVal, band: ping.band, score: ping.score,
        num: pingHasVal ? fmtMs(pingShown) : dash, unit: "ms",
        wire: showWire ? { kind: "flat", text: "latency — uncompensated" } : null,
      });
    return out;
  });

  // Guided empty state (§14.3): before the first run there's nothing measured,
  // so invite action instead of leaving three bare dashes unexplained. Only the
  // idle pointer lives here — the warmup "checking your connection" status is
  // already shown below the gauge, so we don't duplicate it above the footer.
  const guidance = $derived.by(() => {
    if (store.phase === "idle") return "Your results appear here once you press Engage.";
    return "";
  });
</script>

{#snippet metricCard(c: CardVM)}
  <article class="chip" class:active={c.active}>
    <header>
      <span class="ico {c.ico}">{@html c.icon}</span>
      {#if c.term}
        <span class="label term" use:tooltip={JARGON.ping}>{c.label}</span>
      {:else}
        <span class="label">{c.label}</span>
      {/if}
      {#if c.showPip}
        <span
          class="pip pip-{c.band}"
          use:tooltip={`Measurement stability: ${Math.round(c.score * 100)}%`}>{c.band}</span
        >
      {/if}
    </header>
    <div class="val">
      <span class="num">{c.num}</span>
      <span class="unit">{c.unit}</span>
    </div>
    {#if c.sub}
      <div class="sub">{c.sub}</div>
    {/if}
    {#if c.wire}
      <div class="est">
        {#if c.wire.kind === "lift"}
          <span class="est-arrow">→</span>
          <span class="est-num">{c.wire.num}</span>
          <span class="est-tag" use:tooltip={JARGON.wireRate}>wire {c.wire.pct}</span>
        {:else}
          <span class="est-flat">{c.wire.text}</span>
        {/if}
      </div>
    {/if}
  </article>
{/snippet}

<div class="chips" class:reserve={store.phase !== "idle"}>
  {#each cards as c (c.key)}
    {@render metricCard(c)}
  {/each}
</div>

<!-- Guided empty state (§14.3) — a quiet invitation while there's no data. -->
{#if guidance}
  <p class="metric-guidance">{guidance}</p>
{/if}

<style>
  /* Flex so only the currently-visible cards share the width (progressive
     reveal): one card spans full width, two split in half, three in thirds. */
  /* Wrap on actual available width (not a viewport breakpoint): when the stage
     is narrow — e.g. panels docked — the cards re-split 3 → 2 → 1 instead of
     squeezing past their content. */
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
  }
  /* Reserve one card-row of height for the whole run (from warmup on) so the
     progressive reveal of cards doesn't resize the gauge above it — the dial
     stays put from warmup through testing instead of snapping smaller when the
     first card appears. */
  .chips.reserve {
    min-height: 92px;
  }
  /* Stack into a single column on the narrow single-column shell (<760px).
     In the column the chips must size to their content (header + value +
     optional estimate) — flex:1 1 0 with the min-height floor would otherwise
     let a chip shrink below its content and clip the bottom padding. */
  @media (max-width: 759px) {
    .chips {
      flex-direction: column;
    }
    .chip {
      flex: 0 0 auto;
      min-width: 0;
    }
  }

  .chip {
    flex: 1 1 180px;
    min-width: 150px;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-height: 92px;
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
    transition:
      border-color var(--dur-hover) var(--ease-out),
      transform var(--dur-hover) var(--ease-out);
  }
  .chip:hover {
    transform: translateY(-1px);
    border-color: var(--border-strong);
  }

  /* Staggered enter (§13.7): the three cards fade/rise in sequence (0–120ms)
     on mount. Decorative — gated on no-preference so reduced-motion users get
     them instantly (and the global §4.5 guard further neutralizes it). */
  @media (prefers-reduced-motion: no-preference) {
    .chip {
      animation: chip-enter 220ms var(--ease-out) both;
    }
    .chip:nth-child(1) {
      animation-delay: 0ms;
    }
    .chip:nth-child(2) {
      animation-delay: 60ms;
    }
    .chip:nth-child(3) {
      animation-delay: 120ms;
    }
    .chip:nth-child(4) {
      animation-delay: 180ms;
    }
  }
  @keyframes chip-enter {
    from {
      opacity: 0;
      transform: translateY(5px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
  /* The live/active metric gains a faint brand ring (mirrors §3.8). */
  .chip.active {
    border-color: color-mix(in srgb, var(--brand) 46%, var(--border));
    box-shadow:
      var(--elev-tile),
      0 0 0 1px color-mix(in srgb, var(--brand) 30%, transparent);
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .ico {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--r-well);
    border: 1px solid var(--border);
    background: var(--surface-2);
  }
  .ico :global(svg) {
    width: 15px;
    height: 15px;
  }
  .ico.dl {
    color: var(--phase-download);
    border-color: color-mix(in srgb, var(--phase-download) 34%, var(--border));
  }
  .ico.ul {
    color: var(--phase-upload);
    border-color: color-mix(in srgb, var(--phase-upload) 34%, var(--border));
  }
  .ico.pg {
    color: var(--phase-latency);
    border-color: color-mix(in srgb, var(--phase-latency) 34%, var(--border));
  }
  .ico.bd {
    color: var(--phase-bidirectional);
    border-color: color-mix(in srgb, var(--phase-bidirectional) 34%, var(--border));
  }
  .label {
    font-size: var(--type-sm);
    font-weight: 700;
    letter-spacing: -0.01em;
    color: var(--text);
  }
  /* Jargon-term affordance (§14.3) — dotted underline cues a hover/focus tooltip. */
  .label.term {
    cursor: help;
    text-decoration: underline dotted color-mix(in srgb, var(--text-soft) 70%, transparent);
    text-underline-offset: 3px;
  }
  .label.term:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: 2px;
    border-radius: var(--radius-xs);
  }

  /* Confidence pip — fixed slot, never reflows the row. */
  .pip {
    margin-left: auto;
    padding: 2px 7px;
    border-radius: 999px;
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .pip-high {
    background: var(--ok-soft);
    color: var(--ok);
  }
  .pip-medium {
    background: var(--warn-soft);
    color: var(--warn);
  }
  .pip-low {
    background: var(--err-soft);
    color: var(--err);
  }

  .val {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  /* Results stand out: the value uses the Space Grotesk display face (tabular
     so it doesn't shift), echoing the hero number a tier down. */
  .num {
    font-family: var(--font-display);
    font-variant-numeric: tabular-nums;
    font-feature-settings: "tnum" 1;
    font-size: var(--type-xl);
    font-weight: 600;
    letter-spacing: var(--track-tight);
    color: var(--text);
    line-height: 1;
  }
  .unit {
    font-family: var(--font-mono);
    font-size: var(--type-xs);
    font-weight: 700;
    color: var(--text-soft);
  }

  /* Per-direction subline (bidirectional card) — quiet mono detail under the
     combined headline. */
  .sub {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 11px;
    color: var(--text-soft);
    letter-spacing: 0.01em;
  }

  /* Estimate line — always reserved (fixed height) so toggling the
     estimate on/off never shifts layout. */
  .est {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-height: 16px;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 12px;
  }
  .est-arrow {
    color: var(--text-soft);
  }
  .est-num {
    font-weight: 700;
    color: var(--brand-strong);
  }
  .est-tag {
    color: var(--text-soft);
    font-size: 10px;
    letter-spacing: 0.02em;
  }
  .est-flat {
    color: var(--text-soft);
    font-size: 11px;
  }

  /* Guided empty-state line (§14.3) — quiet invitation while there's no data. */
  .metric-guidance {
    margin: 8px 0 0;
    text-align: center;
    font-size: 12px;
    line-height: 1.4;
    color: var(--text-soft);
  }

</style>
