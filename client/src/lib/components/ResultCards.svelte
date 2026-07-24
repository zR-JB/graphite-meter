<script lang="ts">
  // Result cards for latency, transfer, and bidirectional stages. Each card
  // blends live readings with finalized per-stage results.
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { ICON } from "../constants";
  import { tooltip, JARGON } from "../actions/tooltip";

  interface Props {
    compact?: boolean;
  }
  let { compact = false }: Props = $props();

  const dash = "—";

  function transferModel(phase: "download" | "upload") {
    const stability = store.liveStability[phase];
    if (store.phase === phase) {
      const live = store.liveCompensation;
      return {
        measuredBytesPerSec: live.measuredBytesPerSec,
        estimatedBytesPerSec: live.estimatedBytesPerSec,
        lowerBytesPerSec: live.lowerBytesPerSec,
        upperBytesPerSec: live.upperBytesPerSec,
        available: live.available,
        multiplier: live.totalMultiplier,
        band: stability?.band ?? "low",
        score: stability?.score ?? 0,
        active: true,
        has: live.measuredBytesPerSec > 0,
      };
    }
    const stageResult = store.stageResults[phase];
    const compensation =
      phase === "download"
        ? store.downloadCompensation
        : store.uploadCompensation;
    return {
      measuredBytesPerSec: stageResult?.reportedBytesPerSec ?? 0,
      estimatedBytesPerSec: compensation.estimatedBytesPerSec,
      lowerBytesPerSec: compensation.lowerBytesPerSec,
      upperBytesPerSec: compensation.upperBytesPerSec,
      available: compensation.available,
      multiplier: compensation.totalMultiplier,
      band: stageResult?.band ?? stability?.band ?? "low",
      score: stageResult?.stabilityScore ?? stability?.score ?? 0,
      active: false,
      has: !!stageResult,
    };
  }

  const download = $derived.by(() => transferModel("download"));
  const upload = $derived.by(() => transferModel("upload"));

  const bidi = $derived.by(() => {
    if (store.phase === "bidirectional") {
      const live = store.liveBidirectional ?? { down: 0, up: 0 };
      return {
        down: live.down,
        up: live.up,
        combined: live.down + live.up,
        band: "low" as const,
        score: 0,
        active: true,
        has: live.down + live.up > 0,
      };
    }
    const result = store.result?.bidirectional;
    const down = result?.down.reportedBytesPerSec ?? 0;
    const up = result?.up.reportedBytesPerSec ?? 0;
    return {
      down,
      up,
      combined: down + up,
      band: result?.down.band ?? "low",
      score: result?.down.stabilityScore ?? 0,
      active: false,
      has: !!result,
    };
  });

  const ping = $derived.by(() => {
    const stability = store.liveStability.latency;
    if (store.phase === "latency") {
      return {
        ms: store.liveRtt,
        band: stability?.band ?? "low",
        score: stability?.score ?? 0,
        active: true,
        has: store.liveRtt > 0,
      };
    }
    const stageResult = store.stageResults.latency;
    const reported = stageResult?.reportedMs ?? null;
    return {
      ms: reported ?? store.liveRtt,
      band: stageResult?.band ?? stability?.band ?? "low",
      score: stageResult?.stabilityScore ?? stability?.score ?? 0,
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

  const showPing = $derived(
    store.runConfig.stages.latency && (ping.active || ping.has),
  );
  const showDownload = $derived(
    store.runConfig.stages.download && (download.active || download.has),
  );
  const showUpload = $derived(
    store.runConfig.stages.upload && (upload.active || upload.has),
  );
  const showBidi = $derived(
    store.runConfig.stages.bidirectional && (bidi.active || bidi.has),
  );

  const downloadInUnit = $derived(store.toUnit(download.measuredBytesPerSec));
  const uploadInUnit = $derived(store.toUnit(upload.measuredBytesPerSec));
  const bidiInUnit = $derived(store.toUnit(bidi.combined));

  const showWire = $derived(store.showWireEstimates);

  type CardWire =
    | { kind: "lift"; num: string; pct: string }
    | { kind: "flat"; text: string }
    | null;
  interface CardVM {
    key: string;
    icon: string;
    accent: string; // accent class: dl | ul | bd | pg
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
    available: boolean;
  }): CardWire {
    if (!showWire) return null;
    if (m.has && !m.available)
      return { kind: "flat", text: "loopback — no physical wire" };
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
    model: typeof download,
    hasVal: boolean,
    shown: number,
  ): CardVM {
    const isDownload = phase === "download";
    return {
      key: phase,
      icon: isDownload ? ICON.download : ICON.upload,
      accent: isDownload ? "dl" : "ul",
      label: isDownload ? "Download" : "Upload",
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
    if (showDownload)
      out.push(
        transferCard("download", download, download.has, downloadInUnit),
      );
    if (showUpload)
      out.push(transferCard("upload", upload, upload.has, uploadInUnit));
    if (showBidi)
      out.push({
        key: "bidirectional",
        icon: ICON.bidirectional,
        accent: "bd",
        label: "Bi-dir",
        term: false,
        active: bidi.active,
        hasVal: bidi.has,
        showPip: bidi.has && !bidi.active,
        band: bidi.band,
        score: bidi.score,
        num: bidi.has ? fmtSpeed(bidiInUnit) : dash,
        unit: store.unitLabel,
        sub: bidi.has
          ? `↓ ${fmtSpeed(store.toUnit(bidi.down))}  ↑ ${fmtSpeed(store.toUnit(bidi.up))} ${store.unitLabel}`
          : undefined,
        wire: null,
      });
    if (showPing)
      out.push({
        key: "latency",
        icon: ICON.ping,
        accent: "pg",
        label: "Ping",
        term: true,
        active: ping.active,
        hasVal: ping.has,
        showPip: ping.has,
        band: ping.band,
        score: ping.score,
        num: ping.has ? fmtMs(ping.ms) : dash,
        unit: "ms",
        wire: null,
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
      <span class="ico {c.accent}">{@html c.icon}</span>
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
    <span class="ico {c.accent}">{@html c.icon}</span>
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
  /* Matches .result-card min-height and GaugePanel's results-slot reserve. */
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
    gap: var(--space-2);
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
    outline: var(--focus-ring);
    outline-offset: 2px;
    border-radius: var(--r-well);
  }

  .pip {
    margin-left: auto;
    padding: 2px 7px;
    border-radius: var(--r-full);
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

  /* Guided empty-state line: a quiet invitation while there is no data. */
  .metric-guidance {
    margin: var(--space-2) 0 0;
    text-align: center;
    font-size: 12px;
    line-height: 1.4;
    color: var(--text-soft);
  }

  /* Compact strip: one slim row per finished or active stage, carrying icon,
     label, and number. Earlier stages stay visible while the next one runs.
     No card chrome, pip, or wire-estimate line. */
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
    gap: var(--space-1);
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
