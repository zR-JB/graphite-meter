<script lang="ts">
  import ResultSummary from "./ResultSummary.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { MISSING, STAGE } from "../presentation/vocabulary";
  import type { LiveRateValues } from "../presentation/liveRateAnimator";
  import {
    compensationTooltip,
    type CompensationEstimate,
  } from "../compensation";
  import {
    serverEvidence,
    summaryCards,
    type SummaryEvidence,
    type SummaryStatus,
  } from "../presentation/resultSummary";

  let {
    compact = false,
    liveRates,
  }: {
    compact?: boolean;
    liveRates?: LiveRateValues;
  } = $props();

  const controller = getApplicationController();
  let shown = $state("");
  const details = $derived(store.result?.multiServer);
  const ORDER = ["download", "upload", "bidirectional", "latency"] as const;

  function selectScope(id: string) {
    shown = id;
    if (details?.servers.some((s) => s.server.id === id && s.latencyTarget))
      controller.focusServer(id);
  }
  const rate = (bytesPerSec: number) => ({
    num: fmtSpeed(store.toUnit(bytesPerSec)),
    unit: store.unitLabel,
  });
  const wire = (estimate: CompensationEstimate | null) =>
    store.showWireEstimates && estimate && estimate.totalMultiplier >= 1.005
      ? {
          bytesPerSec: estimate.estimatedBytesPerSec,
          pct: `+${((estimate.totalMultiplier - 1) * 100).toFixed(1)}%`,
          tooltip: compensationTooltip(estimate),
        }
      : null;

  const cards = $derived.by(() => {
    if (compact) return [];
    const status: SummaryEvidence["status"] = Object.fromEntries(
      ORDER.flatMap((key) => {
        const value = store.stagePresentation[key].status;
        return ["complete", "partial", "failed"].includes(value)
          ? [[key, value as SummaryStatus]]
          : [];
      }),
    );
    const evidence =
      (details && shown && serverEvidence(details, shown, status)) ||
      ({
        status,
        download: store.stageResults.download,
        upload: store.stageResults.upload,
        bidirectional:
          store.result?.bidirectional ??
          store.error?.partial?.bidirectional ??
          null,
        latency: store.stageResults.latency,
        latencyMeasured: true,
        latencySource:
          details && details.selection.length > 1
            ? details.selection.find((s) => s.id === store.latencyFocus)?.name
            : undefined,
        wire: {
          download: wire(store.downloadCompensation),
          upload: wire(store.uploadCompensation),
          bidirectional: wire(store.bidirectionalCompensation),
        },
      } satisfies SummaryEvidence);
    return summaryCards(evidence, rate, store.unitBase);
  });

  // Live chips: earlier stages stay while the next runs. Animated rates are
  // visual only; accessible values use receiver accounting.
  const chips = $derived.by(() =>
    ORDER.flatMap((key) => {
      const { status } = store.stagePresentation[key];
      if (status === "disabled" || status === "pending") return [];
      const active = status === "active" || status === "recovering";
      let value: number | null;
      let authoritative: number | null;
      if (key === "latency") {
        value = active
          ? store.liveRtt || null
          : (store.stageResults.latency?.reportedMs ?? null);
        authoritative = value;
      } else if (key === "bidirectional") {
        const live = liveRates ?? store.visualBidirectional;
        const bidi =
          store.result?.bidirectional ?? store.error?.partial?.bidirectional;
        value = active
          ? (live?.down ?? 0) + (live?.up ?? 0)
          : bidi?.down && bidi.up
            ? bidi.down.reportedBytesPerSec + bidi.up.reportedBytesPerSec
            : null;
        authoritative = active
          ? (store.liveBidirectional?.down ?? 0) +
            (store.liveBidirectional?.up ?? 0)
          : value;
      } else {
        value = active
          ? (liveRates?.transfer ?? store.visualTransferBytesPerSec)
          : (store.stageResults[key]?.reportedBytesPerSec ?? null);
        authoritative = active ? store.liveTransferBytesPerSec : value;
      }
      if (active && key !== "latency" && !store.aggregateEvidence)
        value = authoritative = null;
      const timeout = key === "latency" && active && store.liveLatencyLost;
      const format = (n: number | null) =>
        n === null ? MISSING : key === "latency" ? fmtMs(n) : rate(n).num;
      return [
        {
          key,
          active,
          label: STAGE[key].short,
          icon: STAGE[key].icon,
          num: timeout ? MISSING : format(value),
          accessibleNum: timeout ? "probe timeout" : format(authoritative),
          unit:
            key === "latency" ? (timeout ? "timeout" : "ms") : store.unitLabel,
        },
      ];
    }),
  );
</script>

{#if compact}
  <div class="result-chips">
    {#each chips as c (c.key)}
      <div class="result-chip enter" class:active={c.active} data-tone={c.key}>
        <span class="tone-icon">{@html c.icon}</span>
        <span class="chip-label">{c.label}</span>
        <span class="chip-val" aria-hidden={c.active ? "true" : undefined}>
          <span class="num">{c.num}</span>
          <span class="unit">{c.unit}</span>
        </span>
        {#if c.active}
          <span class="sr-only">{c.label}: {c.accessibleNum} {c.unit}</span>
        {/if}
      </div>
    {/each}
  </div>
{:else}
  <ResultSummary {cards} {details} scope={shown} onscope={selectScope} />
{/if}

<style>
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
    font-weight: var(--w-heavy);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip-val {
    display: flex;
    align-items: baseline;
    gap: var(--space-1);
  }
  .chip-val .num {
    font: var(--w-heavy) var(--type-sm) var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .chip-val .unit {
    color: var(--text-soft);
    font: var(--w-heavy) var(--type-2xs) var(--font-mono);
  }
</style>
