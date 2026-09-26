<script lang="ts">
  import ServerTag from "./ServerTag.svelte";
  import ResultServerContext from "./ResultServerContext.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { MISSING, STAGE } from "../presentation/vocabulary";
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
  const dash = MISSING;
  const stages = (
    ["download", "upload", "bidirectional", "latency"] as const
  ).map((key) => ({ key, icon: STAGE[key].icon, label: STAGE[key].short }));
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
          num: lost ? dash : hasValue ? format(value!) : dash,
          accessibleNum: lost
            ? "probe timeout"
            : hasValue
              ? format(authoritative!)
              : dash,
          unit: key === "latency" ? (lost ? "timeout" : "ms") : store.unitLabel,
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
            sub = "Not measured";
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
  <article class="surface result-card">
    <header>
      <span class="tone-icon" data-tone={c.key}>{@html c.icon}</span>
      {#if c.key === "latency"}
        <span class="label term" use:tooltip={JARGON.latency}>{c.label}</span>
      {:else}
        <span class="label">{c.label}</span>
      {/if}
      {#if c.hasValue && c.status === "complete"}
        <span
          class="badge"
          data-tone={c.band === "high"
            ? "ok"
            : c.band === "medium"
              ? "warn"
              : "err"}
          use:tooltip={`Measurement stability: ${Math.round(c.score * 100)}%`}
          aria-label={`Measurement stability: ${Math.round(c.score * 100)}%, ${c.band}`}
          >{c.band}</span
        >
      {/if}
      {#if c.status === "partial"}
        <span class="badge" data-tone="err">Partial</span>
      {:else if c.status === "failed"}
        <span class="badge" data-tone="err">Failed</span>
      {/if}
    </header>
    {#key selectedServer}<div class="result-readout enter">
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
            <span class="term" use:tooltip={JARGON.jitter}>jitter</span>
            {#if c.key === "latency" && details && details.selection.length > 1}
              <div class="result-source">
                <ServerTag
                  servers={details.selection}
                  id={scoped ? selectedServer : store.latencyFocus}
                  label="Latency source"
                />
              </div>
            {/if}
          </div>
        {/if}
        {#if c.wire}
          <div class="est">
            <span class="est-num">{c.wire.num}</span>
            <span class="term" use:tooltip={c.wire.tooltip}
              >wire {c.wire.pct}</span
            >
          </div>
        {/if}
      </div>{/key}
    {#if c.sub}
      <div class="sub">
        {c.sub}{#if c.hasValue}<span class="sr-only"> {c.unit}</span>{/if}
      </div>
    {/if}
  </article>
{/snippet}

{#snippet resultChip(c: (typeof readouts)[number])}
  <div class="result-chip enter" class:active={c.active}>
    <span class="tone-icon" data-tone={c.key}>{@html c.icon}</span>
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
  <div
    class="result-bank"
    style:max-width={`${Math.min(4, cards.length) * 280}px`}
  >
    {#if details && details.selection.length > 1}<div class="result-context">
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
  </div>
{/if}

<style>
  .result-bank {
    margin-inline: auto;
    container: results / inline-size;
  }
  .result-context {
    margin-bottom: var(--space-2);
  }
  .result-cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: var(--space-2);
  }
  @container results (max-width: 452px) {
    .result-cards {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .result-card:last-child:nth-child(odd) {
      grid-column: 1 / -1;
    }
  }
  @container results (max-width: 301px) {
    .result-cards {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .result-card {
    display: grid;
    align-content: start;
    gap: 6px;
    min-width: 0;
    min-height: 64px;
    padding: 10px var(--space-3);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  header .badge {
    margin-left: auto;
  }
  .label {
    font-size: var(--type-sm);
    font-weight: 700;
    letter-spacing: -0.01em;
  }
  .result-readout {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }
  .val,
  .est,
  .jitter {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .num {
    font: 600 var(--type-xl) / 1 var(--font-display);
    font-variant-numeric: tabular-nums;
    letter-spacing: var(--track-tight);
  }
  .unit {
    color: var(--text-soft);
    font: 700 var(--type-xs) var(--font-mono);
  }
  .est,
  .jitter {
    min-height: 16px;
    font: var(--type-sm) var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .est-num,
  .jitter-num {
    color: var(--brand-strong);
    font-weight: 700;
  }
  .jitter-unit,
  .result-readout .term {
    color: var(--text-soft);
    font-size: var(--type-2xs);
  }
  .result-source {
    min-width: 0;
    max-width: 40%;
    margin-left: auto;
  }
  .sub {
    color: var(--text-soft);
    font: var(--type-xs) var(--font-mono);
    font-variant-numeric: tabular-nums;
  }

  /* Compact strip: one slim row per finished or active stage. Earlier stages
     stay visible while the next one runs; no card chrome or verdicts. */
  .result-chips {
    display: grid;
    gap: var(--space-1);
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
    border-color: var(--brand-line);
  }
  .result-chip .tone-icon {
    width: 20px;
    height: 20px;
  }
  .chip-label {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    color: var(--text-soft);
    font-size: var(--type-xs);
    font-weight: 700;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip-val {
    display: flex;
    align-items: baseline;
    gap: var(--space-1);
  }
  .chip-val .num {
    font: 700 var(--type-sm) var(--font-mono);
  }
  .chip-val .unit {
    font-size: var(--type-2xs);
  }
</style>
