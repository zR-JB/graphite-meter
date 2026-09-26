<script lang="ts">
  import Icon from "./Icon.svelte";
  import ResultSummary from "./ResultSummary.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { MISSING, STAGE } from "../presentation/vocabulary";
  import type { RatePair } from "../presentation/liveRateAnimator";
  import {
    liveWire,
    summaryCards,
    summaryEvidence,
  } from "../presentation/resultSummary";

  let {
    compact = false,
    liveRates,
  }: {
    compact?: boolean;
    liveRates?: RatePair | null;
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
  const wire = (estimate: Parameters<typeof liveWire>[0]) =>
    store.showWireEstimates ? liveWire(estimate) : null;

  const cards = $derived.by(() => {
    if (compact) return [];
    const stages = Object.fromEntries(
      ORDER.map((key) => [key, store.stagePresentation[key].status]),
    ) as Record<(typeof ORDER)[number], string>;
    const evidence = summaryEvidence(
      stages,
      {
        download: store.stageResults.download,
        upload: store.stageResults.upload,
        bidirectional: store.result?.bidirectional ?? null,
        latency: store.stageResults.latency,
        added: store.result?.bufferbloat ?? null,
        wire: {
          download: wire(store.downloadCompensation),
          upload: wire(store.uploadCompensation),
          bidirectional: wire(store.bidirectionalCompensation),
        },
      },
      details,
      shown,
      details?.latencyFocus,
    );
    return summaryCards(evidence, rate, store.unitBase);
  });

  // Animated rates are visual only; accessible values use receiver accounting.
  function chipValues(
    key: (typeof ORDER)[number],
    active: boolean,
  ): [shown: number | null, accessible: number | null] {
    if (key === "latency") {
      const ms = active
        ? store.liveRtt || null
        : (store.stageResults.latency?.reportedMs ?? null);
      return [ms, ms];
    }
    if (active) {
      const { down = null, up = null } = store.live ?? {};
      const measured =
        down == null && up == null ? null : (down ?? 0) + (up ?? 0);
      return [liveRates ? liveRates.down + liveRates.up : null, measured];
    }
    const bidi = store.result?.bidirectional;
    const result =
      key !== "bidirectional"
        ? (store.stageResults[key]?.reportedBytesPerSec ?? null)
        : bidi?.down && bidi.up
          ? bidi.down.reportedBytesPerSec + bidi.up.reportedBytesPerSec
          : null;
    return [result, result];
  }

  // Live chips hold a row for every stage from the start, so none appears later.
  const chips = $derived.by(() =>
    ORDER.flatMap((key) => {
      const { status } = store.stagePresentation[key];
      if (status === "disabled") return [];
      const active = status === "active" || status === "recovering";
      const [value, authoritative] = chipValues(key, active);
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
        <span class="tone-icon"><Icon name={c.icon} /></span>
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
