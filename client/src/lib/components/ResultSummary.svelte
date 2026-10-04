<script lang="ts">
  import Icon from "./Icon.svelte";
  import StageGraph from "./StageGraph.svelte";
  import LatencyTrace from "./LatencyTrace.svelte";
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
  const ceiling = $derived(scale?.ceiling ?? 0);
  const baseline = $derived(scale?.baseline ?? null);
  const latencyTop = $derived(scale?.latencyTop ?? 0);
  // The latency card's quiet line is its jitter; a transfer's is its wire rate.
  const jitter = (card: SummaryCard) =>
    card.rows.find((row) => row.label === "Jitter")?.value ?? null;
</script>

<div class="result-summary" style:--cards={Math.min(4, cards.length)}>
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
            <LatencyTrace
              points={graph.latency}
              start={graph.start}
              span={graph.span}
              {baseline}
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
  /* Cards sit centred under the chips, one per stage, at a width that keeps four across a 1024 px screen. */
  .result-summary {
    display: grid;
    gap: var(--space-2);
    width: 100%;
    max-width: calc(var(--cards) * 300px);
    margin-inline: auto;
    container: results / inline-size;
  }
  .result-cards {
    display: grid;
    grid-template-columns: repeat(var(--cards), minmax(0, 1fr));
    gap: var(--space-2);
  }
  /* A phone keeps two across; the running card leads and the others fold to their name and value. */
  @container results (max-width: 520px) {
    .result-cards {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
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
  /* A card is a plate washed in its stage's hue from the middle down; the strip sits in that wash. The running
     card takes its hue as its edge. */
  .card {
    --edge: var(--border);
    position: relative;
    display: grid;
    align-content: start;
    gap: 3px;
    min-width: 0;
    padding: var(--space-2) var(--space-3) 10px;
    overflow: hidden;
    border: var(--hairline) solid var(--edge);
    border-radius: var(--r-chrome);
    background:
      linear-gradient(transparent 40%, var(--tone-wash)), var(--surface-1);
    box-shadow: var(--elev-tile);
    transition: var(--transition-control);
  }
  .card:is(.active, .recovering) {
    --edge: color-mix(in oklab, var(--tone) 70%, var(--border));
    box-shadow:
      var(--elev-tile),
      0 0 0 3px var(--tone-wash);
  }
  .card:is(.pending, .not-run) {
    --edge: var(--border-subtle);
  }
  .face {
    display: grid;
    gap: 3px;
    min-width: 0;
  }
  .name {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    height: 22px;
    color: var(--tone-ink);
    font: var(--w-strong) var(--type-sm) / 1 var(--font-sans);
    white-space: nowrap;
  }
  .name .tone-icon {
    width: 20px;
    height: 20px;
  }
  .name .tone-icon :global(svg) {
    width: 11px;
    height: 11px;
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
    gap: 5px;
    min-width: 0;
    font: var(--w-strong) 24px / 26px var(--font-display);
    letter-spacing: var(--track-tight);
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
    gap: 1px;
  }
  .pair .num {
    font-size: 19px;
  }
  .arrow {
    color: var(--tone);
    font: var(--w-normal) var(--type-body) / 1 var(--font-sans);
  }
  .quiet .num {
    color: var(--text-soft);
  }
  .unit {
    color: var(--text-soft);
    font: var(--w-heavy) var(--type-xs) var(--font-mono);
  }
  /* One quiet line: the wire rate, a failure's reason, the latency card's jitter; after a stall, no data. */
  .line {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    min-width: 0;
    height: 15px;
    overflow: hidden;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-xs) / 15px var(--font-sans);
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
  .no-data {
    flex: none;
    margin-left: auto;
  }
  .delta {
    color: var(--tone-ink);
    font-weight: var(--w-strong);
  }
  .reason {
    color: var(--err);
    font-weight: var(--w-strong);
  }
  /* The strip: the stage's shape and its latency replies, in the plate's wash. */
  .strip {
    height: 64px;
    min-height: 0;
    margin-block: 3px 2px;
  }
  /* Facts on one line, each a quiet label and its figure; "—" until known, so nothing moves. */
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 2px var(--space-3);
    min-width: 0;
    min-height: 15px;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-xs) / 15px var(--font-sans);
  }
  .facts.unknown {
    visibility: hidden;
  }
  .facts > div {
    display: inline-flex;
    gap: 4px;
    white-space: nowrap;
  }
  .facts dt {
    color: var(--text-soft);
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
