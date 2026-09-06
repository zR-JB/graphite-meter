<script lang="ts">
  import ResultServerContext from "./ResultServerContext.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { ICON } from "../constants";
  import { tooltip, JARGON } from "../actions/tooltip";
  import { bidirectionalResultPresentation } from "../presentation/bidirectionalResult";
  import type { LiveRateValues } from "../presentation/liveRateAnimator";
  import { compensationTooltip } from "../compensation";

  let {
    compact = false,
    liveRates,
  }: {
    compact?: boolean;
    liveRates?: LiveRateValues;
  } = $props();

  const controller = getApplicationController();
  let selectedServer = $state("");
  const details = $derived(store.result?.multiServer);
  const scoped = $derived(
    details?.servers.find((server) => server.server.id === selectedServer),
  );
  const results = $derived(
    scoped
      ? {
          download: scoped.download,
          upload: scoped.upload,
          latency: scoped.latency,
        }
      : store.stageResults,
  );
  function selectResult(id: string) {
    selectedServer = id;
    if (
      details?.servers.some(
        (server) => server.server.id === id && server.latencyTarget,
      )
    )
      controller.focusServer(id);
  }
  const dash = "—";
  const stages = [
    { key: "download", icon: ICON.download, accent: "dl", label: "Download" },
    { key: "upload", icon: ICON.upload, accent: "ul", label: "Upload" },
    {
      key: "bidirectional",
      icon: ICON.bidirectional,
      accent: "bd",
      label: "Bi-dir",
    },
    { key: "latency", icon: ICON.ping, accent: "pg", label: "Ping" },
  ] as const;
  const bidirectionalEvidence = $derived(
    scoped
      ? scoped.bidirectional
      : (store.result?.bidirectional ?? store.error?.partial?.bidirectional),
  );
  const bidirectional = $derived(
    bidirectionalResultPresentation(
      bidirectionalEvidence?.down?.reportedBytesPerSec,
      bidirectionalEvidence?.up?.reportedBytesPerSec,
    ),
  );

  // Retain earlier stages during warmup, abort and failure as well as live runs.
  // Animated rates are visual only; accessible values use receiver accounting.
  const readouts = $derived.by(() =>
    stages.flatMap((stage) => {
      const { key } = stage;
      let { status } = store.stagePresentation[key];
      if (scoped && (status === "complete" || status === "partial")) {
        const hasResult =
          key === "bidirectional"
            ? scoped.bidirectional?.down || scoped.bidirectional?.up
            : scoped[key];
        const failed = details?.failures.some(
          (failure) =>
            failure.serverId === scoped.server.id &&
            failure.stage === key &&
            failure.scope === (key === "latency" ? "latency" : "throughput"),
        );
        status =
          key === "latency" && !scoped.latencyTarget
            ? "complete"
            : failed
              ? hasResult
                ? "partial"
                : "failed"
              : hasResult
                ? "complete"
                : "partial";
      }
      if (status === "disabled" || status === "pending") return [];
      const active = status === "active" || status === "recovering";
      let value: number | null;
      let authoritative: number | null;
      if (key === "latency") {
        value = active ? store.liveRtt : (results.latency?.reportedMs ?? null);
        authoritative = value;
      } else if (key === "bidirectional") {
        const live = liveRates ?? store.visualBidirectional;
        value = active
          ? (live?.down ?? 0) + (live?.up ?? 0)
          : bidirectional.combinedBytesPerSec;
        authoritative = active
          ? (store.liveBidirectional?.down ?? 0) +
            (store.liveBidirectional?.up ?? 0)
          : value;
      } else {
        value = active
          ? (liveRates?.transfer ?? store.visualTransferBytesPerSec)
          : (results[key]?.reportedBytesPerSec ?? null);
        authoritative = active ? store.liveTransferBytesPerSec : value;
      }
      if (active && key !== "latency" && !store.aggregateEvidence) {
        value = null;
        authoritative = null;
      }
      const hasValue =
        value !== null && (key !== "latency" || !active || value > 0);
      const lost = key === "latency" && active && store.liveLatencyLost;
      const format = (n: number) =>
        key === "latency" ? fmtMs(n) : fmtSpeed(store.toUnit(n));
      return [
        {
          ...stage,
          status,
          active,
          num: lost ? "lost" : hasValue ? format(value!) : dash,
          accessibleNum: lost
            ? "lost"
            : hasValue
              ? format(authoritative!)
              : dash,
          unit: key === "latency" ? (lost ? "" : "ms") : store.unitLabel,
        },
      ];
    }),
  );

  // Only the completed view needs confidence, directional detail and wire estimates.
  const cards = $derived.by(() =>
    compact
      ? []
      : readouts.map((row) => {
          const { key, status } = row;
          let score = 0;
          let band: "low" | "medium" | "high" = "low";
          let sub: string | undefined;
          let jitterMs: number | null | undefined;
          let compensation;
          if (key === "latency") {
            const result = results.latency;
            score = result?.stabilityScore ?? 0;
            band = result?.band ?? "low";
            if (!scoped && details && details.selection.length > 1)
              sub = `To ${details.selection.find((server) => server.id === store.latencyFocus)?.name ?? "selected server"}`;
            jitterMs =
              result?.reportedMs != null
                ? (result.jitterMs ?? null)
                : undefined;
          } else {
            let stabilityPct: number | undefined;
            if (key === "bidirectional") {
              const { down, up, combinedBytesPerSec, survivingDirection } =
                bidirectional;
              stabilityPct =
                bidirectionalEvidence?.down && bidirectionalEvidence.up
                  ? Math.min(
                      bidirectionalEvidence.down.stabilityPct,
                      bidirectionalEvidence.up.stabilityPct,
                    )
                  : undefined;
              const downText = fmtSpeed(store.toUnit(down ?? 0));
              const upText = fmtSpeed(store.toUnit(up ?? 0));
              sub =
                combinedBytesPerSec !== null
                  ? `↓ ${downText} ↑ ${upText}`
                  : survivingDirection === "down"
                    ? `↓ ${downText} ${store.unitLabel} — upload unavailable`
                    : survivingDirection === "up"
                      ? `↑ ${upText} ${store.unitLabel} — download unavailable`
                      : undefined;
              compensation = store.bidirectionalCompensation;
            } else {
              stabilityPct = results[key]?.stabilityPct;
              compensation =
                key === "download"
                  ? store.downloadCompensation
                  : store.uploadCompensation;
            }
            score = Math.max(0, Math.min(1, (stabilityPct ?? 0) / 100));
            band = score >= 0.9 ? "high" : score >= 0.75 ? "medium" : "low";
          }
          if (scoped && key === "latency" && !scoped.latencyTarget)
            sub = "Not measured for this server";
          const hasValue = row.num !== dash;
          const wire =
            !scoped &&
            store.showWireEstimates &&
            hasValue &&
            status === "complete" &&
            compensation &&
            compensation.totalMultiplier >= 1.005
              ? {
                  tooltip: compensationTooltip(compensation),
                  num: fmtSpeed(
                    store.toUnit(compensation.estimatedBytesPerSec),
                  ),
                  pct: `+${((compensation.totalMultiplier - 1) * 100).toFixed(1)}%`,
                }
              : null;
          return { ...row, score, band, sub, jitterMs, wire, hasValue };
        }),
  );
</script>

{#snippet resultCard(c: (typeof cards)[number])}
  <article class="result-card">
    <header>
      <span class="ico {c.accent}">{@html c.icon}</span>
      {#if c.key === "latency"}
        <span class="label term" use:tooltip={JARGON.ping}>{c.label}</span>
      {:else}
        <span class="label">{c.label}</span>
      {/if}
      {#if c.hasValue && c.status === "complete"}
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
      <div class="val">
        <span class="num">{c.num}</span>
        <span class="unit">{c.unit}</span>
      </div>
      {#if c.jitterMs !== undefined}
        <div class="jitter">
          <span class="jitter-num"
            >{c.jitterMs === null ? dash : fmtMs(c.jitterMs)}
            <span class="jitter-unit">ms</span></span
          >
          <span class="jitter-term" use:tooltip={JARGON.jitter}>jitter</span>
        </div>
      {/if}
      {#if c.wire}
        <div class="est">
          <span class="est-num">{c.wire.num}</span>
          <span class="est-tag" use:tooltip={c.wire.tooltip}
            >wire {c.wire.pct}</span
          >
        </div>
      {/if}
    </div>
    {#if c.sub}
      <div class="sub">
        {c.sub}{#if c.hasValue}<span class="sr-only"> {c.unit}</span>{/if}
      </div>
    {/if}
  </article>
{/snippet}

{#snippet resultChip(c: (typeof readouts)[number])}
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
    {#each readouts as c (c.key)}
      {@render resultChip(c)}
    {/each}
  </div>
{:else}
  {#if details && details.selection.length > 1}<div
      class="result-context"
      style:max-width={`${Math.min(4, cards.length) * 280}px`}
    >
      <ResultServerContext
        {details}
        value={selectedServer}
        onchange={selectResult}
      />
    </div>{/if}
  <div class="result-cards">
    {#each cards as c (c.key)}
      {@render resultCard(c)}
    {/each}
  </div>
{/if}

<style>
  .result-context {
    max-width: 1120px;
    margin: 0 auto var(--space-2);
  }
  .result-cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: var(--space-2);
    min-height: 64px;
    max-width: 840px;
    margin-inline: auto;
  }
  .result-cards:has(> :last-child:nth-child(4)) {
    max-width: 1120px;
  }
  .result-cards:has(> :last-child:nth-child(1)) {
    max-width: 280px;
  }
  .result-cards:has(> :last-child:nth-child(2)) {
    max-width: 560px;
  }

  .result-card {
    min-width: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    align-content: start;
    gap: 6px;
    min-height: 64px;
    padding: 10px var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  .result-card header,
  .result-readout,
  .sub {
    grid-column: 1 / -1;
  }
  .result-readout {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
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

  .est,
  .jitter {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-height: 16px;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 12px;
  }
  .est-num,
  .jitter-num {
    font-weight: 700;
    color: var(--brand-strong);
  }
  .jitter-unit {
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 600;
  }
  .est-tag,
  .jitter-term {
    cursor: help;
    color: var(--text-soft);
    font-size: 10px;
    letter-spacing: 0.02em;
    text-decoration: underline dotted
      color-mix(in srgb, var(--text-soft) 70%, transparent);
    text-underline-offset: 3px;
  }
  .est-tag:focus-visible,
  .jitter-term:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
    border-radius: var(--r-well);
  }
  /* Compact strip: one slim row per finished or active stage, carrying icon,
     label, and number. Earlier stages stay visible while the next one runs.
     No card chrome, confidence verdict, or wire-estimate line. */
  .result-chips {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    width: 100%;
    max-width: 600px;
    margin-inline: auto;
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
