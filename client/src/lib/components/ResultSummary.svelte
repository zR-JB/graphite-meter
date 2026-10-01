<script lang="ts">
  import StageGraph from "./StageGraph.svelte";
  import {
    cardFacts,
    cardNoData,
    cardTip,
    type CardScale,
    type SummaryCard,
    type SummaryRow,
  } from "../presentation/resultSummary";
  import type { MultiServerResult } from "../runner/measure";
  import type { TransportRole } from "../runner/contract";
  import { termAction, tooltipAction, tipGroup } from "../actions/tooltip";
  import {
    MISSING,
    STAGE,
    STATUS,
    STATUS_TONE,
  } from "../presentation/vocabulary";

  let {
    cards,
    scale = null,
    head = null,
    fade = 1,
    details,
    issues = [],
    scope = "",
    running = false,
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
    /** A run is under way: on a phone only the running card stays open. */
    running?: boolean;
  } = $props();
  // A failed transfer's reason sits on its card's line, named by server when several ran, so it never adds a row.
  const attributed = $derived((details?.selection.length ?? 1) > 1 && !scope);
  const listed = $derived(issues.filter((issue) => !issue.throughput.length));
  const reasonOn = (key: TransportRole) =>
    issues
      .filter((issue) => issue.throughput.includes(key))
      .map(({ server, reason }) =>
        attributed
          ? `${server}: ${reason.charAt(0).toLowerCase()}${reason.slice(1)}`
          : reason,
      )
      .join(", ");
  // Latency has its own card; this row holds the transfers.
  const transfers = $derived(cards.filter((card) => card.key !== "latency"));
  const ceiling = $derived(scale?.ceiling ?? 0);
  const baseline = $derived(scale?.baseline ?? null);
  const latencyTop = $derived(scale?.latencyTop ?? 0);
</script>

<div class="result-summary">
  <div class="result-cards" class:running data-tip-group {@attach tipGroup}>
    {#each transfers as card (card.key)}
      {@const graph = card.graph}
      {@const quiet = card.status === "pending" || card.status === "not-run"}
      {@const tone = STATUS_TONE[card.status as keyof typeof STATUS_TONE]}
      {@const lanes = card.rows.filter(
        (row): row is SummaryRow & { stage: TransportRole } => !!row.stage,
      )}
      {@const reason = reasonOn(card.key)}
      {@const facts = cardFacts(card)}
      {@const noData = cardNoData(card)}
      <article
        class="card stage-area {card.status}"
        data-tone={card.key}
        style:--fade={fade}
      >
        <header class="card-head">
          <span class="dot" aria-hidden="true"></span>
          <h3 use:tooltipAction={cardTip(card)}>
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
                  ><span class="sr-only">{STAGE[lane.stage].short}</span><span
                    class="num">{lane.short}</span
                  ></span
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
              ><span class="label" use:termAction={wire.tip}>Wire</span>
              {wire.value}
              <span class="delta">{wire.overhead}</span></span
            >
          {/if}
          {#if noData && !quiet}
            <span class="no-data"
              ><span class="label" use:termAction={noData.tip!}>No data</span>
              {noData.value}</span
            >
          {/if}
        </div>
        {#if graph && scale}
          <div class="graph-slot">
            <StageGraph
              tone={card.key as "download" | "upload" | "bidirectional"}
              lanes={graph.lanes}
              laneNames={card.key === "bidirectional"
                ? [STAGE.download.short, STAGE.upload.short]
                : []}
              latency={graph.latency}
              start={graph.start}
              span={graph.span}
              head={head?.key === card.key ? head : null}
              {ceiling}
              {baseline}
              {latencyTop}
              rate={scale.rate}
              label="{STAGE[card.key].label} over time"
            />
          </div>
        {/if}
        <dl
          class="facts"
          class:unknown={facts.every((row) => row.value === MISSING)}
        >
          {#each facts as row (row.label)}
            <div>
              <dt use:tooltipAction={row.tip ?? ""}>
                {row.label}
              </dt>
              <dd class:quiet={row.value === MISSING}>{row.value}</dd>
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
  /* A card is its stage's area (`.stage-area`): a rule and a wash, no box. Stretched by a taller
     neighbour, its rows stay put and the room falls below them. */
  .card {
    display: grid;
    grid-template-rows: 20px auto 18px;
    grid-auto-rows: auto;
    align-content: start;
    gap: 6px;
    min-width: 0;
    padding: var(--space-3) var(--space-4) var(--space-3);
  }
  /* A phone stacks the cards, so an empty line has nothing to align with. */
  @container results (max-width: 520px) {
    .result-cards {
      grid-template-columns: minmax(0, 1fr);
      row-gap: var(--space-3);
    }
    .card {
      grid-template-rows: 20px;
    }
    /* The running card stays in view, first under the run button: one that waits, or is done while the run goes
       on, is its name and value. */
    .line:empty,
    .card:is(.pending, .not-run) > :is(.line, .graph-slot, .facts),
    .running .card:not(.active, .recovering) > :is(.line, .graph-slot, .facts) {
      display: none;
    }
    .running .card:is(.active, .recovering) {
      order: -1;
    }
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
  /* On the title's baseline (app.css, --role-label). */
  .state {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin: calc(var(--type-md) - var(--type-sm)) 0 0 auto;
    /* Muted, not soft: small text on a running card's wash keeps 4.5:1. */
    color: var(--text-muted);
    font: var(--w-normal) var(--type-sm) / 1 var(--font-sans);
  }
  .headline,
  .line,
  .facts,
  .graph-slot {
    opacity: var(--fade);
  }
  /* Light numerals read as measured values, not as headings. */
  .headline {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
    font: 300 clamp(32px, 2.6vw, 46px) / 1 var(--font-display);
    white-space: nowrap;
  }
  /* A strut one value tall, so a bidirectional pair's smaller figures sit on its baseline and keep the card's height. */
  .headline::after {
    content: "\200b";
  }
  .num {
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
  .reason,
  .wire {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .label {
    color: var(--text-soft);
  }
  /* After a stall, how long no data came, at the line's end. */
  .no-data {
    flex: none;
    margin-left: auto;
    padding-left: var(--space-3);
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
    height: clamp(64px, 11svh, 132px);
    min-height: 0;
  }
  /* A card with no data yet keeps its graph's room but draws nothing in it: only its rule, name and "—". */
  .card:is(.pending, .not-run) > .graph-slot {
    visibility: hidden;
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 5.25rem), 1fr));
    gap: var(--space-2) var(--space-3);
    padding-top: var(--space-2);
    border-top: var(--hairline) solid var(--border-subtle);
  }
  /* Under the dial on a landscape page, which fits one screen down to 1024 x 768, a card's rows sit closer. */
  @media (orientation: landscape) {
    @container viz (min-width: 760px) {
      .card {
        gap: var(--space-1);
      }
      .facts {
        column-gap: var(--space-2);
      }
    }
  }
  /* Until one fact is known the row keeps its place unseen, so the first values never move the instrument. */
  .facts.unknown {
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
  /* "—" for a value not yet measured is soft; a measured value is full ink. */
  .facts dd.quiet {
    color: var(--text-soft);
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
