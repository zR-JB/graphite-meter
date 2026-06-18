<script lang="ts">
  /* ============================================================
   * <MetricChips> — live measured-vs-estimated cards (§13.3)
   * Replaces the Download/Upload/Ping placeholder grid in the
   * center stage. Per metric it shows the MEASURED browser value
   * and, beneath it, the COMPENSATED wire-rate estimate plus a
   * small confidence pip (high/medium/low). Latency (Ping) has no
   * wire-rate compensation, so it shows the live RTT only.
   *
   * Reactivity (§13.3): the live cards read the store's O(1)
   * `liveCompensation` derived (protocol/config multipliers on the
   * latest bps); the post-run cards read `downloadCompensation` /
   * `uploadCompensation`, which recompute only when `result`
   * changes. This component never iterates sample arrays.
   *
   * Zero layout shift: tabular-nums + fixed-band fmtSpeed, fixed
   * pip width, and a fixed two-line value block so the estimate
   * line is always reserved even when absent.
   * ============================================================ */
  import { untrack } from "svelte";
  import { console as store } from "../state/console.svelte";
  import { fmtSpeed, fmtMs, countUp } from "../format";
  import { ICON } from "../constants";
  import { tooltip, JARGON } from "../actions/tooltip";

  const dash = "—";

  // Honour the user's motion preference once (the count-up tween is decorative).
  const reduced =
    typeof window !== "undefined" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  /** Is the named transfer phase the live, currently-running one? */
  function isLivePhase(p: "download" | "upload"): boolean {
    return store.phase === p;
  }

  // ---- Download card ----
  const dl = $derived.by(() => {
    const live = isLivePhase("download");
    if (live) {
      const comp = store.liveCompensation;
      return {
        measuredBps: comp.measuredBps,
        estimatedBps: comp.estimatedBps,
        multiplier: comp.totalMultiplier,
        confidence: comp.confidence,
        active: true,
        has: comp.measuredBps > 0,
      };
    }
    const res = store.result?.download ?? null;
    const comp = store.downloadCompensation;
    return {
      measuredBps: res?.meanBps ?? 0,
      estimatedBps: comp.estimatedBps,
      multiplier: comp.totalMultiplier,
      confidence: comp.confidence,
      active: false,
      has: !!res,
    };
  });

  // ---- Upload card ----
  const ul = $derived.by(() => {
    const live = isLivePhase("upload");
    if (live) {
      const comp = store.liveCompensation;
      return {
        measuredBps: comp.measuredBps,
        estimatedBps: comp.estimatedBps,
        multiplier: comp.totalMultiplier,
        confidence: comp.confidence,
        active: true,
        has: comp.measuredBps > 0,
      };
    }
    const res = store.result?.upload ?? null;
    const comp = store.uploadCompensation;
    return {
      measuredBps: res?.meanBps ?? 0,
      estimatedBps: comp.estimatedBps,
      multiplier: comp.totalMultiplier,
      confidence: comp.confidence,
      active: false,
      has: !!res,
    };
  });

  // ---- Ping card (no wire-rate compensation) ----
  const ping = $derived.by(() => {
    if (store.phase === "latency") {
      return { ms: store.liveRtt, active: true, has: store.liveRtt > 0 };
    }
    const p50 = store.result?.latency?.p50Ms ?? null;
    return { ms: p50 ?? store.liveRtt, active: false, has: p50 != null };
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
  let cancels: Array<() => void> = [];

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

  $effect(() => {
    // Re-run only when `result` flips (becomes available / cleared on reset).
    // The live "from" values are read untracked so per-tick live updates don't
    // retrigger this effect mid-run.
    const res = store.result;
    for (const c of cancels) c();
    cancels = [];

    if (!res) {
      dlSnap = null;
      ulSnap = null;
      pingSnap = null;
      return;
    }

    untrack(() => {
      if (res.download) {
        cancels.push(
          tweenTo(store.toUnit(dl.measuredBps), store.toUnit(res.download.meanBps), (v) => (dlSnap = v)),
        );
      }
      if (res.upload) {
        cancels.push(
          tweenTo(store.toUnit(ul.measuredBps), store.toUnit(res.upload.meanBps), (v) => (ulSnap = v)),
        );
      }
      if (res.latency) {
        cancels.push(tweenTo(ping.ms, res.latency.p50Ms, (v) => (pingSnap = v)));
      }
    });

    return () => {
      for (const c of cancels) c();
      cancels = [];
    };
  });

  // The number each card actually renders: the tweened snap when present,
  // otherwise the live derived measured value.
  const dlShown = $derived(dlSnap ?? store.toUnit(dl.measuredBps));
  const ulShown = $derived(ulSnap ?? store.toUnit(ul.measuredBps));
  const pingShown = $derived(pingSnap ?? ping.ms);

  /* ---- Progressive disclosure of the wire-rate estimate (§14.2) ----
     The headline numbers are always the plainly MEASURED values. The
     compensated wire-rate is a refinement, surfaced on the result cards only
     when the user opts in via the persisted Workbench setting. */
  const showWire = $derived(store.showWireEstimates);

  // Guided empty state (§14.3): before the first run there's nothing measured,
  // so invite action instead of leaving three bare dashes unexplained.
  const guidance = $derived.by(() => {
    if (store.phase === "idle") return "Your results appear here once you press Engage.";
    if (store.phase === "warmup") return "Checking your connection…";
    return "";
  });
</script>

<div class="chips">
  <!-- DOWNLOAD -->
  <article class="chip" class:active={dl.active}>
    <header>
      <span class="ico dl">{@html ICON.download}</span>
      <span class="label">Download</span>
      {#if dl.has && lifted(dl.multiplier)}
        <span class="pip pip-{dl.confidence}" title="Estimate confidence: {dl.confidence}"
          >{dl.confidence}</span
        >
      {/if}
    </header>
    <div class="val">
      <span class="num">{dl.has ? fmtSpeed(dlShown) : dash}</span>
      <span class="unit">{store.unitLabel}</span>
    </div>
    {#if showWire}
      <div class="est">
        {#if dl.has && lifted(dl.multiplier)}
          <span class="est-arrow">→</span>
          <span class="est-num">{fmtSpeed(store.toUnit(dl.estimatedBps))}</span>
          <span class="est-tag" use:tooltip={JARGON.wireRate}>wire {pctLift(dl.multiplier)}</span>
        {:else}
          <span class="est-flat">{dl.has ? "no overhead applied" : ""}</span>
        {/if}
      </div>
    {/if}
  </article>

  <!-- UPLOAD -->
  <article class="chip" class:active={ul.active}>
    <header>
      <span class="ico ul">{@html ICON.upload}</span>
      <span class="label">Upload</span>
      {#if ul.has && lifted(ul.multiplier)}
        <span class="pip pip-{ul.confidence}" title="Estimate confidence: {ul.confidence}"
          >{ul.confidence}</span
        >
      {/if}
    </header>
    <div class="val">
      <span class="num">{ul.has ? fmtSpeed(ulShown) : dash}</span>
      <span class="unit">{store.unitLabel}</span>
    </div>
    {#if showWire}
      <div class="est">
        {#if ul.has && lifted(ul.multiplier)}
          <span class="est-arrow">→</span>
          <span class="est-num">{fmtSpeed(store.toUnit(ul.estimatedBps))}</span>
          <span class="est-tag" use:tooltip={JARGON.wireRate}>wire {pctLift(ul.multiplier)}</span>
        {:else}
          <span class="est-flat">{ul.has ? "no overhead applied" : ""}</span>
        {/if}
      </div>
    {/if}
  </article>

  <!-- PING (latency — no compensation) -->
  <article class="chip" class:active={ping.active}>
    <header>
      <span class="ico pg">{@html ICON.ping}</span>
      <span class="label term" use:tooltip={JARGON.ping}>Ping</span>
    </header>
    <div class="val">
      <span class="num">{ping.has ? fmtMs(pingShown) : dash}</span>
      <span class="unit">ms</span>
    </div>
    {#if showWire}
      <div class="est">
        <span class="est-flat">latency — uncompensated</span>
      </div>
    {/if}
  </article>
</div>

<!-- Guided empty state (§14.3) — a quiet invitation while there's no data. -->
{#if guidance}
  <p class="metric-guidance">{guidance}</p>
{/if}

<style>
  .chips {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 12px;
  }
  /* Stack into a single column on the narrow single-column shell (<760px). */
  @media (max-width: 759px) {
    .chips {
      grid-template-columns: 1fr;
    }
  }

  .chip {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 92px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    box-shadow: var(--shadow-card);
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
      var(--shadow-card),
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
    border-radius: var(--radius-sm);
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
  .label {
    font-size: 12px;
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
  .num {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 22px;
    font-weight: 700;
    letter-spacing: -0.03em;
    color: var(--text);
    line-height: 1;
  }
  .unit {
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 700;
    color: var(--text-soft);
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
