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
  import { console as store } from "../state/console.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { ICON } from "../constants";

  const dash = "—";

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
      <span class="num">{dl.has ? fmtSpeed(store.toUnit(dl.measuredBps)) : dash}</span>
      <span class="unit">{store.unitLabel}</span>
    </div>
    <div class="est">
      {#if dl.has && lifted(dl.multiplier)}
        <span class="est-arrow">→</span>
        <span class="est-num">{fmtSpeed(store.toUnit(dl.estimatedBps))}</span>
        <span class="est-tag">wire {pctLift(dl.multiplier)}</span>
      {:else}
        <span class="est-flat">{dl.has ? "no overhead applied" : ""}</span>
      {/if}
    </div>
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
      <span class="num">{ul.has ? fmtSpeed(store.toUnit(ul.measuredBps)) : dash}</span>
      <span class="unit">{store.unitLabel}</span>
    </div>
    <div class="est">
      {#if ul.has && lifted(ul.multiplier)}
        <span class="est-arrow">→</span>
        <span class="est-num">{fmtSpeed(store.toUnit(ul.estimatedBps))}</span>
        <span class="est-tag">wire {pctLift(ul.multiplier)}</span>
      {:else}
        <span class="est-flat">{ul.has ? "no overhead applied" : ""}</span>
      {/if}
    </div>
  </article>

  <!-- PING (latency — no compensation) -->
  <article class="chip" class:active={ping.active}>
    <header>
      <span class="ico pg">{@html ICON.ping}</span>
      <span class="label">Ping</span>
    </header>
    <div class="val">
      <span class="num">{ping.has ? fmtMs(ping.ms) : dash}</span>
      <span class="unit">ms</span>
    </div>
    <div class="est">
      <span class="est-flat">latency — uncompensated</span>
    </div>
  </article>
</div>

<style>
  .chips {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 16px;
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
    gap: 8px;
    min-height: 120px;
    padding: 14px;
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
    font-size: 26px;
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
</style>
