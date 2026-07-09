<script lang="ts">
  import { untrack } from "svelte";
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs, countUp } from "../format";
  import { ICON } from "../constants";
  import { tooltip, JARGON } from "../actions/tooltip";

  interface Props {
    compact?: boolean;
  }
  let { compact = false }: Props = $props();

  const dash = "—";

  const reduced =
    typeof window !== "undefined" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

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
    const comp =
      phase === "download"
        ? store.downloadCompensation
        : store.uploadCompensation;
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

  function lifted(multiplier: number): boolean {
    return multiplier > 1.0005;
  }

  function pctLift(multiplier: number): string {
    return `+${((multiplier - 1) * 100).toFixed(1)}%`;
  }

  const TWEEN_MS = 220;
  let dlSnap = $state<number | null>(null);
  let ulSnap = $state<number | null>(null);
  let pingSnap = $state<number | null>(null);
  let bidiSnap = $state<number | null>(null);
  let dlCancel: (() => void) | null = null;
  let ulCancel: (() => void) | null = null;
  let pingCancel: (() => void) | null = null;
  let bidiCancel: (() => void) | null = null;

  let pingFrozen = $state<number | null>(null);
  let dlFrozen = $state<number | null>(null);
  let ulFrozen = $state<number | null>(null);
  let bidiFrozen = $state<number | null>(null);

  // Freeze the last live value at phase exit so the final result can tween from
  // what the user just saw, not from zero or a later unrelated sample.
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
    const r = store.stageResults.download;
    dlCancel?.();
    dlCancel = null;
    if (!r) {
      dlSnap = null;
      return;
    }
    dlCancel = untrack(() =>
      tweenTo(
        dlFrozen ?? dl.measuredBytesPerSec,
        r.reportedBytesPerSec,
        (v) => (dlSnap = v),
      ),
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
      tweenTo(
        ulFrozen ?? ul.measuredBytesPerSec,
        r.reportedBytesPerSec,
        (v) => (ulSnap = v),
      ),
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
    pingCancel = untrack(() =>
      tweenTo(pingFrozen ?? ping.ms, r.reportedMs, (v) => (pingSnap = v)),
    );
    return () => {
      pingCancel?.();
      pingCancel = null;
    };
  });

  $effect(() => {
    const r = store.result?.bidirectional;
    bidiCancel?.();
    bidiCancel = null;
    if (!r) {
      bidiSnap = null;
      return;
    }
    const finalCombined = r.down.reportedBytesPerSec + r.up.reportedBytesPerSec;
    bidiCancel = untrack(() =>
      tweenTo(
        bidiFrozen ?? bidi.combined,
        finalCombined,
        (v) => (bidiSnap = v),
      ),
    );
    return () => {
      bidiCancel?.();
      bidiCancel = null;
    };
  });

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
    else if (p === "download") dlFrozen = dl.measuredBytesPerSec;
    else if (p === "upload") ulFrozen = ul.measuredBytesPerSec;
    else if (p === "bidirectional") bidiFrozen = bidi.combined;
  });

  const pingShow = $derived(
    store.config.stages.latency &&
      (ping.active || pingFrozen != null || ping.has),
  );
  const dlShow = $derived(
    store.config.stages.download && (dl.active || dlFrozen != null || dl.has),
  );
  const ulShow = $derived(
    store.config.stages.upload && (ul.active || ulFrozen != null || ul.has),
  );
  const bidiShow = $derived(
    store.config.stages.bidirectional &&
      (bidi.active || bidiFrozen != null || bidi.has),
  );

  const pingHasVal = $derived(
    ping.active ? ping.has : ping.has || pingFrozen != null,
  );
  const dlHasVal = $derived(dl.active ? dl.has : dl.has || dlFrozen != null);
  const ulHasVal = $derived(ul.active ? ul.has : ul.has || ulFrozen != null);
  const bidiHasVal = $derived(
    bidi.active ? bidi.has : bidi.has || bidiFrozen != null,
  );

  const dlShown = $derived(
    store.toUnit(
      dlSnap ??
        (dl.active
          ? dl.measuredBytesPerSec
          : (dlFrozen ?? dl.measuredBytesPerSec)),
    ),
  );
  const ulShown = $derived(
    store.toUnit(
      ulSnap ??
        (ul.active
          ? ul.measuredBytesPerSec
          : (ulFrozen ?? ul.measuredBytesPerSec)),
    ),
  );
  const pingShown = $derived(
    pingSnap ?? (ping.active ? ping.ms : (pingFrozen ?? ping.ms)),
  );
  const bidiShown = $derived(
    store.toUnit(
      bidiSnap ?? (bidi.active ? bidi.combined : (bidiFrozen ?? bidi.combined)),
    ),
  );

  const showWire = $derived(store.showWireEstimates);

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

  function wireFor(m: {
    has: boolean;
    multiplier: number;
    estimatedBytesPerSec: number;
  }): CardWire {
    if (!showWire) return null;
    if (m.has && lifted(m.multiplier))
      return {
        kind: "lift",
        num: fmtSpeed(store.toUnit(m.estimatedBytesPerSec)),
        pct: pctLift(m.multiplier),
      };
    return { kind: "flat", text: m.has ? "no overhead applied" : "" };
  }

  function transferCard(
    phase: "download" | "upload",
    model: typeof dl,
    hasVal: boolean,
    shown: number,
  ): CardVM {
    const download = phase === "download";
    return {
      key: phase,
      icon: download ? ICON.download : ICON.upload,
      ico: download ? "dl" : "ul",
      label: download ? "Download" : "Upload",
      term: false,
      active: model.active,
      hasVal,
      showPip: hasVal,
      band: model.band,
      score: model.score,
      num: hasVal ? fmtSpeed(shown) : dash,
      unit: store.unitLabel,
      wire: wireFor(model),
    };
  }

  const cards = $derived.by<CardVM[]>(() => {
    const out: CardVM[] = [];
    if (dlShow) out.push(transferCard("download", dl, dlHasVal, dlShown));
    if (ulShow) out.push(transferCard("upload", ul, ulHasVal, ulShown));
    if (bidiShow)
      out.push({
        key: "bidirectional",
        icon: ICON.bidirectional,
        ico: "bd",
        label: "Bi-dir",
        term: false,
        active: bidi.active,
        hasVal: bidiHasVal,
        showPip: bidiHasVal && !bidi.active,
        band: bidi.band,
        score: bidi.score,
        num: bidiHasVal ? fmtSpeed(bidiShown) : dash,
        unit: store.unitLabel,
        sub: bidiHasVal
          ? `↓ ${fmtSpeed(store.toUnit(bidi.down))}  ↑ ${fmtSpeed(store.toUnit(bidi.up))} ${store.unitLabel}`
          : undefined,
        wire: null,
      });
    if (pingShow)
      out.push({
        key: "latency",
        icon: ICON.ping,
        ico: "pg",
        label: "Ping",
        term: true,
        active: ping.active,
        hasVal: pingHasVal,
        showPip: pingHasVal,
        band: ping.band,
        score: ping.score,
        num: pingHasVal ? fmtMs(pingShown) : dash,
        unit: "ms",
        wire: showWire
          ? { kind: "flat", text: "latency — uncompensated" }
          : null,
      });
    return out;
  });

  const guidance = $derived.by(() => {
    if (store.phase === "idle")
      return "Your results appear here once you press Engage.";
    return "";
  });
</script>

{#snippet resultCard(c: CardVM)}
  <article class="result-card" class:active={c.active}>
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
          use:tooltip={`Measurement stability: ${Math.round(c.score * 100)}%`}
          >{c.band}</span
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
          <span class="est-tag" use:tooltip={JARGON.wireRate}
            >wire {c.wire.pct}</span
          >
        {:else}
          <span class="est-flat">{c.wire.text}</span>
        {/if}
      </div>
    {/if}
  </article>
{/snippet}

{#snippet resultChip(c: CardVM)}
  <div class="result-chip" class:active={c.active}>
    <span class="ico {c.ico}">{@html c.icon}</span>
    <span class="chip-label">{c.label}</span>
    <span class="chip-val">
      <span class="num">{c.num}</span>
      <span class="unit">{c.unit}</span>
    </span>
  </div>
{/snippet}

{#if compact}
  <div class="result-chips">
    {#each cards as c (c.key)}
      {@render resultChip(c)}
    {/each}
  </div>
{:else}
  <div class="result-cards" class:reserve={store.phase !== "idle"}>
    {#each cards as c (c.key)}
      {@render resultCard(c)}
    {/each}
  </div>

  {#if guidance}
    <p class="metric-guidance">{guidance}</p>
  {/if}
{/if}

<style>
  .result-cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: var(--space-3);
  }
  /* Keep this in sync with .result-card min-height and GaugePanel's result slot. */
  .result-cards.reserve {
    min-height: 64px;
  }

  .result-card {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 64px;
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
    transition:
      border-color var(--dur-hover) var(--ease-out),
      transform var(--dur-hover) var(--ease-out);
  }
  .result-card:hover {
    transform: translateY(-1px);
    border-color: var(--border-strong);
  }

  @media (prefers-reduced-motion: no-preference) {
    .result-card {
      animation: card-enter 220ms var(--ease-out) both;
    }
    .result-card:nth-child(1) {
      animation-delay: 0ms;
    }
    .result-card:nth-child(2) {
      animation-delay: 60ms;
    }
    .result-card:nth-child(3) {
      animation-delay: 120ms;
    }
    .result-card:nth-child(4) {
      animation-delay: 180ms;
    }
  }
  @keyframes card-enter {
    from {
      opacity: 0;
      transform: translateY(5px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
  .result-card.active {
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
    width: 22px;
    height: 22px;
    border-radius: var(--r-well);
    border: 1px solid var(--border);
    background: var(--surface-2);
  }
  .ico :global(svg) {
    width: 13px;
    height: 13px;
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
    border-color: color-mix(
      in srgb,
      var(--phase-bidirectional) 34%,
      var(--border)
    );
  }
  .label {
    font-size: var(--type-sm);
    font-weight: 700;
    letter-spacing: -0.01em;
    color: var(--text);
  }
  .label.term {
    cursor: help;
    text-decoration: underline dotted
      color-mix(in srgb, var(--text-soft) 70%, transparent);
    text-underline-offset: 3px;
  }
  .label.term:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: 2px;
    border-radius: var(--radius-xs);
  }

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

  .sub {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 11px;
    color: var(--text-soft);
    letter-spacing: 0.01em;
  }

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

  /* Guided empty-state line — quiet invitation while there's no data. */
  .metric-guidance {
    margin: 8px 0 0;
    text-align: center;
    font-size: 12px;
    line-height: 1.4;
    color: var(--text-soft);
  }

  /* ---- Compact strip (mobile-first "see earlier stages while the next one
     runs") ---- One slim row per finished/active stage: icon + label + number,
     no card chrome, no pip, no wire-estimate line. Deliberately smaller by
     construction rather than a breakpoint-shrunk full card. */
  .result-chips {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .result-chip {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-height: 28px;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
  }
  .result-chip.active {
    border-color: color-mix(in srgb, var(--brand) 46%, var(--border));
  }
  .result-chip .ico {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: var(--r-well);
    border: 1px solid var(--border);
    background: var(--surface-2);
    flex: none;
  }
  .result-chip .ico :global(svg) {
    width: 12px;
    height: 12px;
  }
  .chip-label {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--type-xs);
    font-weight: 700;
    color: var(--text-soft);
  }
  .chip-val {
    flex: none;
    display: flex;
    align-items: baseline;
    gap: 4px;
  }
  .chip-val .num {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--type-sm);
    font-weight: 700;
    color: var(--text);
  }
  .chip-val .unit {
    font-family: var(--font-mono);
    font-size: 10px;
    font-weight: 700;
    color: var(--text-soft);
  }
</style>
