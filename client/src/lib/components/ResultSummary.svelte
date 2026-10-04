<script lang="ts">
  import Icon from "./Icon.svelte";
  import StageGraph from "./StageGraph.svelte";
  import { latencyScale } from "../presentation/scales";
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
    columns = null,
  }: {
    cards: SummaryCard[];
    /** Strips share one rate and latency scale so their heights compare. */
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
    /** The stage keys' columns; a stage with a key and no card keeps its column with a quiet placeholder. */
    columns?: SummaryCard["key"][] | null;
  } = $props();
  const column = (key: SummaryCard["key"]) =>
    columns ? columns.indexOf(key) + 1 : null;
  const off = $derived(
    columns?.filter((key) => !cards.some((card) => card.key === key)) ?? [],
  );
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
  const ceiling = $derived(scale?.ceiling ?? 0);
  const baseline = $derived(scale?.baseline ?? null);
  const latencyTop = $derived(scale?.latencyTop ?? 0);
  // The latency card's quiet line is its jitter; a transfer's is its wire rate.
  const jitter = (card: SummaryCard) =>
    card.rows.find((row) => row.label === "Jitter")?.value ?? null;
</script>

<div
  class="result-summary"
  style:--cards={columns ? columns.length : Math.min(4, cards.length)}
>
  <div class="result-cards" class:running data-tip-group {@attach tipGroup}>
    {#each cards as card (card.key)}
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
        class="card {card.status}"
        data-tone={card.key}
        style:--fade={fade}
        data-col={column(card.key)}
      >
        <span class="face" use:tooltipAction={cardTip(card)}>
          <span class="name">
            <span class="tone-icon" aria-hidden="true"
              ><Icon name={card.icon} /></span
            >
            <h3>{STAGE[card.key].label}</h3>
            {#if tone || card.status === "active"}<span class="status"
                >{#if tone && tone !== "neutral"}<span
                    class="status-dot"
                    data-tone={tone}
                  ></span>{/if}{card.status === "active"
                  ? STATUS.running
                  : STATUS[card.status as keyof typeof STATUS]}</span
              >{/if}
          </span>
          {#key scope}
            <span
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
              {#if card.unit && !quiet}<span class="unit">{card.unit}</span
                >{/if}
            </span>
          {/key}
        </span>
        <span class="line">
          {#if reason && !quiet}
            <span class="reason">{reason}</span>
          {:else if card.wire}
            {@const wire = card.wire}
            <span class="wire"
              ><span class="label" use:termAction={wire.tip}>wire</span>
              {wire.value}
              <span class="delta">{wire.overhead}</span></span
            >
          {:else if card.key === "latency" && jitter(card) && !quiet}
            <span class="wire"
              ><span class="label">jitter</span> {jitter(card)}</span
            >
          {/if}
          {#if noData && !quiet}
            <span class="no-data"
              ><span class="label" use:termAction={noData.tip!}>no data</span>
              {noData.value}</span
            >
          {/if}
        </span>
        {#if graph && scale && card.key === "latency"}
          <div class="strip">
            <StageGraph
              tone="latency"
              lanes={[]}
              latency={graph.latency}
              start={graph.start}
              span={graph.span}
              ceiling={1}
              {baseline}
              latencyTop={latencyScale(graph.latency.map((point) => point.ms))}
              rate={scale.rate}
              label="Idle latency over time"
            />
          </div>
        {:else if graph && scale}
          <div class="strip">
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
        {#if facts.length}
          <dl
            class="facts"
            class:unknown={facts.every((row) => row.value === MISSING)}
          >
            {#each facts as row (row.label)}
              <div>
                <dt use:tooltipAction={row.tip ?? ""}>{row.label}</dt>
                <dd class:quiet={row.value === MISSING}>{row.value}</dd>
              </div>
            {/each}
          </dl>
        {/if}
        {#if card.accessible}<span class="sr-only">{card.accessible}</span>{/if}
      </article>
    {/each}
    {#each off as key (key)}
      <article class="card off" data-tone={key} data-col={column(key)}>
        <span class="name">
          <span class="tone-icon" aria-hidden="true"
            ><Icon name={STAGE[key].icon} /></span
          >
          <h3>{STAGE[key].label}</h3>
        </span>
        <span class="off-note">{STATUS.off}</span>
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
  /* One card per stage in the keys' columns; the strips share one scale, so a stage's shape compares. */
  .result-summary {
    display: grid;
    grid-template-rows: minmax(0, 1fr) auto;
    gap: var(--space-2);
    width: 100%;
    height: 100%;
    container: results / inline-size;
  }
  .result-cards {
    display: grid;
    grid-template-columns: repeat(var(--cards), minmax(0, 1fr));
    gap: var(--space-4);
    min-height: 0;
  }
  @container results (max-width: 1100px) {
    .result-cards {
      gap: var(--space-3);
    }
  }
  /* A narrow console keeps two across; a phone leads with the running card and folds the others to their name
     and value. */
  @container results (max-width: 720px) {
    .result-cards {
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: var(--space-2);
    }
  }
  /* Above a narrow console each card takes its key's column; narrower, they flow two across. */
  @container results (min-width: 721px) {
    .card[data-col="1"] {
      grid-column: 1;
    }
    .card[data-col="2"] {
      grid-column: 2;
    }
    .card[data-col="3"] {
      grid-column: 3;
    }
    .card[data-col="4"] {
      grid-column: 4;
    }
  }
  @container results (max-width: 520px) {
    .running .card:not(.active, .recovering) > :is(.line, .strip, .facts),
    .card:is(.pending, .not-run) > :is(.line, .strip, .facts) {
      display: none;
    }
    .running .card:is(.active, .recovering) {
      order: -1;
    }
    .result-cards > :global(:last-child:nth-child(odd)) {
      grid-column: 1 / -1;
    }
  }
  /* A card is a panel ruled in its stage's hue along the top; the running card's edge strengthens. In its row it
     takes the row's height up to a limit, its strip growing with it. */
  .card {
    --edge: var(--border);
    position: relative;
    display: grid;
    align-content: start;
    gap: var(--space-1);
    min-width: 0;
    max-height: 300px;
    padding: 10px var(--space-4) var(--space-3);
    overflow: hidden;
    border: var(--hairline) solid var(--edge);
    border-top: 2px solid var(--tone);
    border-radius: var(--r-surface);
    background: var(--surface-1);
    transition: var(--transition-control);
  }
  .card:has(> .strip) {
    grid-template-rows: auto auto minmax(68px, 1fr) auto;
  }
  .card:is(.active, .recovering) {
    --edge: color-mix(in oklab, var(--tone) 55%, var(--border));
  }
  .card:is(.pending, .not-run, .off) {
    --edge: var(--border-subtle);
    border-top-color: color-mix(in oklab, var(--tone) 45%, transparent);
  }
  /* A stage with a key and no card keeps its column: its name, and why there is nothing under it. */
  .card.off {
    gap: var(--space-2);
  }
  .off-note {
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 16px var(--font-sans);
  }
  .face {
    display: grid;
    gap: var(--space-1);
    min-width: 0;
  }
  .name {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    height: 20px;
    color: var(--text);
    font: var(--role-title);
    white-space: nowrap;
  }
  .name .tone-icon {
    width: 18px;
    height: 18px;
  }
  .name .tone-icon :global(svg) {
    width: 10px;
    height: 10px;
  }
  .status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin-left: auto;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-xs) / 1 var(--font-sans);
  }
  .headline,
  .line,
  .facts,
  .strip,
  .status {
    opacity: var(--fade);
  }
  .headline {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    font: var(--role-readout);
    font-size: 26px;
    white-space: nowrap;
  }
  /* A strut one value tall, so a bidirectional pair's smaller figures sit on its baseline and keep the card's height. */
  .headline::after {
    content: "\200b";
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  .pair {
    display: inline-flex;
    align-items: baseline;
    gap: 2px;
  }
  .pair .num {
    font-size: 20px;
  }
  .pair + .pair {
    margin-left: 4px;
  }
  .arrow {
    color: var(--text-soft);
    font: var(--w-normal) var(--type-md) / 1 var(--font-sans);
  }
  .quiet .num {
    color: var(--text-soft);
  }
  .unit {
    color: var(--text-muted);
    font: var(--role-figure-sm);
    line-height: 1;
  }
  /* One quiet line: the wire rate, a failure's reason, the latency card's jitter; after a stall, no data. */
  .line {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    min-width: 0;
    height: 16px;
    overflow: hidden;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 16px var(--font-sans);
    white-space: nowrap;
  }
  .reason,
  .wire {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .wire,
  .no-data {
    font: var(--role-figure-sm);
    line-height: 16px;
    color: var(--text-muted);
  }
  .label {
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 16px var(--font-sans);
  }
  .no-data {
    flex: none;
    margin-left: auto;
  }
  .delta {
    color: var(--text);
  }
  .reason {
    color: var(--err);
    font-weight: var(--w-strong);
  }
  /* The strip: the stage's shape and its latency replies, on a field tinted in the stage's hue; it grows with
     the card up to a limit. */
  .strip {
    min-height: 68px;
    max-height: 120px;
    margin-top: var(--space-1);
    padding: 6px 8px 0;
    border-radius: var(--r-well);
    background: color-mix(in oklab, var(--tone) 7%, var(--surface-1));
  }
  /* The facts are ruled rows, a quiet label and its figure on one line; "—" until known, so nothing moves. */
  .facts {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    min-width: 0;
    margin-top: var(--space-2);
    border-top: var(--hairline) solid var(--border-subtle);
    color: var(--text);
    font: var(--role-figure-sm);
  }
  .facts.unknown {
    visibility: hidden;
  }
  .facts > div {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-3);
    height: 22px;
    min-width: 0;
    white-space: nowrap;
  }
  .facts > div + div {
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .facts dt {
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1 var(--font-sans);
  }
  .facts dd {
    font-variant-numeric: tabular-nums;
  }
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
