<script lang="ts">
  import StageGraph from "./StageGraph.svelte";
  import {
    cardTip,
    type CardScale,
    type SummaryCard,
  } from "../presentation/resultSummary";
  import type { MultiServerResult } from "../runner/measure";
  import type { TransportRole } from "../runner/contract";
  import { term, tipGroup, tooltip } from "../actions/tooltip";
  import { STAGE, STATUS, STATUS_TONE } from "../presentation/vocabulary";

  let {
    cards,
    scale = null,
    head = null,
    fade = 1,
    details,
    issues = [],
    scope = "",
  }: {
    cards: SummaryCard[];
    /** Graphs share one rate and latency scale so their heights compare. */
    scale?: CardScale | null;
    /** The running stage's leading edge. */
    head?: { key: string; t: number; values: (number | null)[] } | null;
    fade?: number;
    details?: MultiServerResult | null;
    issues?: {
      server: string;
      stages: string;
      reason: string;
      throughput: TransportRole[];
    }[];
    scope?: string;
  } = $props();
  // One shown server's reasons sit on the card they failed; only All servers needs an attributed list.
  const attributed = $derived((details?.selection.length ?? 1) > 1 && !scope);
  const onCards = $derived(
    attributed ? [] : issues.filter((issue) => issue.throughput.length),
  );
  const listed = $derived(
    attributed ? issues : issues.filter((issue) => !issue.throughput.length),
  );
  // Latency has its own card; this row holds the transfers.
  const transfers = $derived(cards.filter((card) => card.key !== "latency"));
  // Facts read the same way on every card: what the link peaked at, how steady it was, what moved.
  const FACT_ORDER = ["Peak", "Stability", "Down + up", "Transferred"];
  const facts = (card: SummaryCard) =>
    card.rows
      .filter((row) => !row.stage)
      .toSorted(
        (a, b) => FACT_ORDER.indexOf(a.label) - FACT_ORDER.indexOf(b.label),
      );
</script>

<div class="result-summary">
  <div class="result-cards" data-tip-group {@attach tipGroup}>
    {#each transfers as card (card.key)}
      {@const quiet = card.status === "pending" || card.status === "not-run"}
      {@const tone = STATUS_TONE[card.status as keyof typeof STATUS_TONE]}
      {@const lanes = card.rows.filter((row) => row.stage)}
      {@const reason = onCards.find((issue) =>
        issue.throughput.includes(card.key),
      )?.reason}
      <article
        class="card {card.status}"
        data-tone={card.key}
        style:--fade={fade}
      >
        <header class="card-head">
          <span class="dot" aria-hidden="true"></span>
          <h3 {@attach tooltip(() => cardTip(card))}>
            {STAGE[card.key].label}
          </h3>
          {#if tone || card.status === "active"}<span class="state"
              >{#if tone && tone !== "neutral"}<span
                  class="status-dot"
                  data-tone={tone}
                ></span>{/if}{card.status === "active"
                ? STATUS.running
                : STATUS[card.status as keyof typeof STATUS]}</span
            >{/if}
        </header>
        {#key scope}
          <div
            class="headline enter"
            class:quiet
            aria-hidden={card.accessible ? "true" : undefined}
          >
            {#if card.key === "bidirectional" && lanes.length === 2 && !quiet}
              {#each lanes as lane (lane.stage)}
                <span class="pair" data-tone={lane.stage}
                  ><span class="arrow" aria-hidden="true"
                    >{lane.stage === "download" ? "↓" : "↑"}</span
                  ><span class="num">{lane.short}</span></span
                >
              {/each}
            {:else}
              <span class="num">{card.num}</span>
            {/if}
            {#if card.unit && !quiet}<span class="unit">{card.unit}</span>{/if}
          </div>
        {/key}
        <div class="line">
          {#if reason && !quiet}
            <span class="reason">{reason}</span>
          {:else if card.wire}
            {@const wire = card.wire}
            <span class="wire"
              ><span class="label" {@attach term(() => wire.tip)}>Wire</span>
              {wire.value}
              <span class="delta">{wire.overhead}</span></span
            >
          {/if}
        </div>
        {#if card.graph && scale}
          <div class="graph-slot">
            <StageGraph
              tone={card.key as "download" | "upload" | "bidirectional"}
              lanes={card.graph.lanes}
              laneNames={card.key === "bidirectional"
                ? [STAGE.download.short, STAGE.upload.short]
                : []}
              latency={card.graph.latency}
              start={card.graph.start}
              span={card.graph.span}
              head={head?.key === card.key ? head : null}
              ceiling={scale.ceiling}
              baseline={scale.baseline}
              latencyTop={scale.latencyTop}
              rate={scale.rate}
              label="{STAGE[card.key].label} over time"
            />
          </div>
        {/if}
        <dl class="facts">
          {#each facts(card) as row (row.label)}
            <div>
              <dt>{row.label}</dt>
              <dd>{row.value}</dd>
            </div>
          {/each}
        </dl>
        {#if card.accessible}<span class="sr-only">{card.accessible}</span>{/if}
      </article>
    {/each}
  </div>
  {#if listed.length}
    <dl class="issues enter" class:attributed aria-label="Issues">
      {#each listed as issue, index (index)}
        {#if attributed}<dt>{issue.server}</dt>
          <dd>{issue.stages}</dd>
        {:else}<dt>{issue.stages}</dt>{/if}
        <dd class="reason">{issue.reason}</dd>
      {/each}
    </dl>
  {/if}
</div>

<style>
  .result-summary {
    display: grid;
    gap: var(--space-3);
    width: 100%;
    container: results / inline-size;
  }
  .result-cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: var(--space-4) var(--space-5);
  }
  /* A card is its stage's light on the page: a rule in its hue and a wash that fades out, no box. */
  .card {
    --wash: 0%;
    display: grid;
    grid-template-rows: 20px auto 18px;
    grid-auto-rows: auto;
    gap: 6px;
    min-width: 0;
    padding: var(--space-3) var(--space-4) var(--space-3);
    border-top: 2px solid
      color-mix(in oklab, var(--tone) var(--rule, 100%), transparent);
    background: linear-gradient(
      180deg,
      color-mix(in oklab, var(--tone) var(--wash), transparent),
      transparent 78%
    );
    transition:
      --wash var(--dur-graph) var(--ease-out),
      border-color var(--dur-graph) var(--ease-out);
  }
  .card:is(.complete, .partial, .failed, .stopped) {
    --wash: 9%;
  }
  .card.active {
    --wash: 16%;
  }
  .card:is(.pending, .not-run) {
    --rule: 30%;
  }
  .card-head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: var(--r-full);
    background: var(--tone);
  }
  h3 {
    color: var(--text);
    font: var(--w-strong) var(--type-md) / 20px var(--font-sans);
    white-space: nowrap;
  }
  .state {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin-left: auto;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1 var(--font-sans);
  }
  .headline,
  .line,
  .facts,
  .graph-slot {
    opacity: var(--fade);
  }
  .headline {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
    white-space: nowrap;
  }
  /* Light numerals read as measured values, not as headings. */
  .num {
    font: 300 clamp(32px, 2.6vw, 46px) / 1 var(--font-display);
    font-variant-numeric: tabular-nums;
    letter-spacing: -0.025em;
  }
  .pair {
    display: inline-flex;
    align-items: baseline;
    gap: 2px;
  }
  .pair .num {
    font-size: clamp(26px, 2vw, 36px);
  }
  .arrow {
    color: var(--tone);
    font: var(--w-normal) var(--type-lg) / 1 var(--font-sans);
  }
  .quiet .num {
    color: var(--text-soft);
  }
  .unit {
    color: var(--text-muted);
    font: var(--w-normal) var(--type-md) / 1 var(--font-sans);
  }
  .line {
    display: flex;
    align-items: baseline;
    min-width: 0;
    overflow: hidden;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-body) / 18px var(--font-sans);
    white-space: nowrap;
  }
  .wire .label {
    color: var(--text-soft);
  }
  .delta {
    margin-left: 4px;
    color: var(--tone-ink);
    font-weight: var(--w-strong);
  }
  .reason {
    color: var(--err);
    font-weight: var(--w-strong);
  }
  .graph-slot {
    height: var(--graph-h, clamp(64px, 11svh, 132px));
    min-height: 0;
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 6.5rem), 1fr));
    gap: var(--space-2) var(--space-3);
    padding-top: var(--space-2);
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .facts:empty {
    visibility: hidden;
  }
  .facts > div {
    display: grid;
    gap: 1px;
    min-width: 0;
  }
  .facts dt {
    overflow: hidden;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1.3 var(--font-sans);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .facts dd {
    font: var(--w-normal) var(--type-md) / 1.3 var(--font-sans);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .issues {
    display: grid;
    grid-template-columns: auto auto;
    justify-content: center;
    gap: 2px var(--space-3);
    color: var(--text-muted);
    font: var(--w-normal) var(--type-sm) / 1.5 var(--font-sans);
  }
  .issues.attributed {
    grid-template-columns: auto auto auto;
  }
  .attributed dt {
    color: var(--text);
    font-weight: var(--w-strong);
  }
  .issues .reason {
    color: var(--text-soft);
    font-weight: var(--w-normal);
  }
</style>
