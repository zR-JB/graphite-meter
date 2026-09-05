<script lang="ts">
  // Result cards for latency, transfer, and bidirectional stages. Each card
  // blends live readings with finalized per-stage results.
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { ICON } from "../constants";
  import { tooltip, JARGON } from "../actions/tooltip";
  import { bidirectionalResultPresentation } from "./bidirectionalResult";
  import type { LiveRateValues } from "../presentation/liveRateAnimator";
  import {
    compensationTooltip,
    type CompensationEstimate,
  } from "../compensation";

  interface Props {
    compact?: boolean;
    liveRates?: LiveRateValues;
  }
  let { compact = false, liveRates }: Props = $props();

  const dash = "—";

  function throughputDisplayStability(stabilityPct: number): {
    score: number;
    band: "low" | "medium" | "high";
  } {
    const score = Math.max(0, Math.min(1, stabilityPct / 100));
    return {
      score,
      band: score >= 0.9 ? "high" : score >= 0.75 ? "medium" : "low",
    };
  }

  function transferModel(phase: "download" | "upload") {
    const presentation = store.stagePresentation[phase];
    const stability = store.liveStability[phase];
    if (
      presentation.status === "active" ||
      presentation.status === "recovering"
    ) {
      const live = store.liveCompensation;
      const measuredBytesPerSec = compact
        ? (liveRates?.transfer ?? store.visualTransferBytesPerSec)
        : live.measuredBytesPerSec;
      return {
        measuredBytesPerSec,
        authoritativeBytesPerSec: store.liveTransferBytesPerSec,
        estimatedBytesPerSec: live.estimatedBytesPerSec,
        available: live.available,
        multiplier: live.totalMultiplier,
        compensation: live,
        band: stability?.band ?? "low",
        score: stability?.score ?? 0,
        active: true,
        has: measuredBytesPerSec > 0,
        status: presentation.status,
      };
    }
    const stageResult = store.stageResults[phase];
    const displayStability = stageResult
      ? throughputDisplayStability(stageResult.stabilityPct)
      : null;
    const compensation =
      phase === "download"
        ? store.downloadCompensation
        : store.uploadCompensation;
    return {
      measuredBytesPerSec: stageResult?.reportedBytesPerSec ?? 0,
      authoritativeBytesPerSec: stageResult?.reportedBytesPerSec ?? 0,
      estimatedBytesPerSec: compensation.estimatedBytesPerSec,
      available: compensation.available,
      multiplier: compensation.totalMultiplier,
      compensation,
      band: displayStability?.band ?? stability?.band ?? "low",
      score: displayStability?.score ?? stability?.score ?? 0,
      active: false,
      has: !!stageResult,
      status: presentation.status,
    };
  }

  const download = $derived.by(() => transferModel("download"));
  const upload = $derived.by(() => transferModel("upload"));

  const bidi = $derived.by(() => {
    const presentation = store.stagePresentation.bidirectional;
    const stability = store.liveStability.bidirectional;
    if (
      presentation.status === "active" ||
      presentation.status === "recovering"
    ) {
      const live = (compact
        ? (liveRates ?? store.visualBidirectional)
        : store.liveBidirectional) ?? { down: 0, up: 0 };
      return {
        down: live.down,
        up: live.up,
        combined: live.down + live.up,
        authoritativeDown: (store.liveBidirectional ?? { down: 0, up: 0 }).down,
        authoritativeUp: (store.liveBidirectional ?? { down: 0, up: 0 }).up,
        band: stability?.band ?? "low",
        score: stability?.score ?? 0,
        active: true,
        has: live.down + live.up > 0,
        status: presentation.status,
        compensation: store.liveBidirectionalCompensation,
      };
    }
    const result = bidirectionalResultPresentation(
      store.result?.bidirectional ??
        store.error?.partial?.bidirectional ??
        null,
    );
    const survivor =
      result.survivingDirection === "down" ? result.down : result.up;
    const downDisplay = result.down
      ? throughputDisplayStability(result.down.stabilityPct)
      : null;
    const upDisplay = result.up
      ? throughputDisplayStability(result.up.stabilityPct)
      : null;
    const combinedDisplay =
      downDisplay && upDisplay
        ? throughputDisplayStability(
            Math.min(result.down!.stabilityPct, result.up!.stabilityPct),
          )
        : null;
    return {
      down: result.down?.reportedBytesPerSec ?? 0,
      up: result.up?.reportedBytesPerSec ?? 0,
      combined: result.combinedBytesPerSec,
      authoritativeDown: result.down?.reportedBytesPerSec ?? 0,
      authoritativeUp: result.up?.reportedBytesPerSec ?? 0,
      survivingDirection: result.survivingDirection,
      band:
        combinedDisplay?.band ??
        survivor?.band ??
        result.down?.band ??
        result.up?.band ??
        "low",
      score: combinedDisplay?.score ?? survivor?.stabilityScore ?? 0,
      active: false,
      has: result.combinedBytesPerSec !== null,
      status: presentation.status,
      compensation: store.bidirectionalCompensation,
    };
  });

  // Below half a percent, the modeled difference is not useful result-card
  // context. The assumptions remain available through the shared disclosure.
  function lifted(multiplier: number): boolean {
    return multiplier >= 1.005;
  }

  function pctLift(multiplier: number): string {
    return `+${((multiplier - 1) * 100).toFixed(1)}%`;
  }

  const ping = $derived.by(() => {
    const presentation = store.stagePresentation.latency;
    const stability = store.liveStability.latency;
    if (
      presentation.status === "active" ||
      presentation.status === "recovering"
    ) {
      return {
        ms: store.liveRtt,
        lost: store.liveLatencyLost,
        band: stability?.band ?? "low",
        score: stability?.score ?? 0,
        active: true,
        has: store.liveRtt > 0,
        status: presentation.status,
      };
    }
    const stageResult = store.stageResults.latency;
    const reported = stageResult?.reportedMs ?? null;
    return {
      ms: reported ?? store.liveRtt,
      lost: false,
      band: stageResult?.band ?? stability?.band ?? "low",
      score: stageResult?.stabilityScore ?? stability?.score ?? 0,
      active: false,
      has: reported != null,
      status: presentation.status,
    };
  });

  const showPing = $derived(
    store.stagePresentation.latency.status !== "disabled" &&
      store.stagePresentation.latency.status !== "pending",
  );
  const showDownload = $derived(
    store.stagePresentation.download.status !== "disabled" &&
      store.stagePresentation.download.status !== "pending",
  );
  const showUpload = $derived(
    store.stagePresentation.upload.status !== "disabled" &&
      store.stagePresentation.upload.status !== "pending",
  );
  const showBidi = $derived(
    store.stagePresentation.bidirectional.status !== "disabled" &&
      store.stagePresentation.bidirectional.status !== "pending",
  );

  const downloadInUnit = $derived(store.toUnit(download.measuredBytesPerSec));
  const uploadInUnit = $derived(store.toUnit(upload.measuredBytesPerSec));
  const bidiInUnit = $derived(
    bidi.combined === null ? null : store.toUnit(bidi.combined),
  );

  const showWire = $derived(store.showWireEstimates);

  type CardWire =
    | { kind: "lift"; num: string; pct: string; tooltip: string }
    | { kind: "na"; tooltip: string }
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
    status:
      | "disabled"
      | "pending"
      | "active"
      | "recovering"
      | "complete"
      | "partial"
      | "failed";
    num: string; // pre-formatted, or the dash
    accessibleNum: string;
    unit: string;
    sub?: string; // per-direction detail (bidirectional only)
    wire: CardWire;
  }

  function wireFor(
    m: {
      has: boolean;
      multiplier: number;
      estimatedBytesPerSec: number;
      available: boolean;
      compensation: CompensationEstimate;
    },
    status: CardVM["status"],
  ): CardWire {
    if (!showWire || !m.has || status !== "complete") return null;
    const tooltip = compensationTooltip(m.compensation);
    if (!m.available) return { kind: "na", tooltip };
    if (lifted(m.multiplier))
      return {
        kind: "lift",
        num: fmtSpeed(store.toUnit(m.estimatedBytesPerSec)),
        pct: pctLift(m.multiplier),
        tooltip,
      };
    return null;
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
      showPip: hasVal && model.status === "complete",
      band: model.band,
      score: model.score,
      status: model.status,
      num: hasVal ? fmtSpeed(shown) : dash,
      accessibleNum: hasVal
        ? fmtSpeed(store.toUnit(model.authoritativeBytesPerSec))
        : dash,
      unit: store.unitLabel,
      wire: wireFor(model, model.status),
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
        showPip: bidi.has && bidi.status === "complete",
        band: bidi.band,
        score: bidi.score,
        status: bidi.status,
        num: bidi.has && bidiInUnit !== null ? fmtSpeed(bidiInUnit) : dash,
        accessibleNum:
          bidi.has && bidiInUnit !== null
            ? fmtSpeed(
                store.toUnit(bidi.authoritativeDown + bidi.authoritativeUp),
              )
            : dash,
        unit: store.unitLabel,
        sub: bidi.has
          ? `↓ ${fmtSpeed(store.toUnit(bidi.down))}  ↑ ${fmtSpeed(store.toUnit(bidi.up))} ${store.unitLabel}`
          : bidi.survivingDirection === "down"
            ? `↓ ${fmtSpeed(store.toUnit(bidi.down))} ${store.unitLabel} — upload unavailable`
            : bidi.survivingDirection === "up"
              ? `↑ ${fmtSpeed(store.toUnit(bidi.up))} ${store.unitLabel} — download unavailable`
              : undefined,
        wire: wireFor(
          {
            has: bidi.has,
            multiplier: bidi.compensation.totalMultiplier,
            estimatedBytesPerSec: bidi.compensation.estimatedBytesPerSec,
            available: bidi.compensation.available,
            compensation: bidi.compensation,
          },
          bidi.status,
        ),
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
        showPip: ping.has && ping.status === "complete",
        band: ping.band,
        score: ping.score,
        status: ping.status,
        num:
          ping.active && ping.lost ? "lost" : ping.has ? fmtMs(ping.ms) : dash,
        accessibleNum:
          ping.active && ping.lost ? "lost" : ping.has ? fmtMs(ping.ms) : dash,
        unit: ping.active && ping.lost ? "" : "ms",
        wire: null,
      });
    return out;
  });

  const guidance = $derived.by(() => {
    if (store.phase === "idle")
      return "Your results appear here once you press Start test.";
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
      {#if c.status === "partial"}
        <span class="partial">Partial</span>
      {:else if c.status === "failed"}
        <span class="partial">Failed</span>
      {/if}
    </header>
    <div class="result-readout">
      <div class="val" aria-hidden={compact && c.active ? "true" : undefined}>
        <span class="num">{c.num}</span>
        <span class="unit">{c.unit}</span>
      </div>
      {#if compact && c.active}
        <span class="sr-only">{c.label}: {c.accessibleNum} {c.unit}</span>
      {/if}
      {#if c.sub}
        <div class="sub" aria-hidden={compact && c.active ? "true" : undefined}>
          {c.sub}
        </div>
      {/if}
      {#if c.wire}
        <div class="est">
          {#if c.wire.kind === "lift"}
            <span class="est-num">{c.wire.num}</span>
            <span class="est-tag" use:tooltip={c.wire.tooltip}
              >wire {c.wire.pct}</span
            >
          {:else}
            <span class="est-tag" use:tooltip={c.wire.tooltip}>wire n/a</span>
          {/if}
        </div>
      {/if}
    </div>
  </article>
{/snippet}

{#snippet resultChip(c: CardVM)}
  <div class="result-chip" class:active={c.active}>
    <span class="ico {c.accent}">{@html c.icon}</span>
    <span class="chip-label">{c.label}</span>
    <span class="chip-val" aria-hidden={c.active ? "true" : undefined}>
      <span class="num">{c.num}</span>
      <span class="unit">{c.unit}</span>
    </span>
    {#if c.active}
      <span class="sr-only">{c.label}: {c.accessibleNum} {c.unit}</span>
    {/if}
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
  /* Keep an empty result grid from shifting adjacent content. */
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
  .result-readout {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  @container viz (min-width: 1000px) {
    .result-card {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      align-items: start;
      gap: 8px;
    }
  }
  .result-card:hover {
    transform: translateY(-1px);
    border-color: var(--border-strong);
  }
  .partial {
    margin-left: auto;
    color: var(--err);
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  @media (prefers-reduced-motion: no-preference) {
    .result-chip {
      animation: quick-content-enter var(--dur-hover) var(--ease-out) both;
    }
    .result-card {
      animation: card-enter var(--dur-slide) var(--ease-out) both;
    }
    .result-card:nth-child(1) {
      animation-delay: 0ms;
    }
    .result-card:nth-child(2) {
      animation-delay: 25ms;
    }
    .result-card:nth-child(3) {
      animation-delay: 50ms;
    }
    .result-card:nth-child(4) {
      animation-delay: 75ms;
    }
  }
  @keyframes quick-content-enter {
    from {
      opacity: 0.65;
      transform: translateY(2px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
  @keyframes card-enter {
    from {
      opacity: 0;
      transform: translateY(3px);
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
  .est-num {
    font-weight: 700;
    color: var(--brand-strong);
  }
  .est-tag {
    cursor: help;
    color: var(--text-soft);
    font-size: 10px;
    letter-spacing: 0.02em;
    text-decoration: underline dotted
      color-mix(in srgb, var(--text-soft) 70%, transparent);
    text-underline-offset: 3px;
  }
  .est-tag:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
    border-radius: var(--r-well);
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
     No card chrome, confidence verdict, or wire-estimate line. */
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
