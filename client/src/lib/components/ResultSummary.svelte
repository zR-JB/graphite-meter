<script lang="ts">
  import Icon from "./Icon.svelte";
  import StageGraph from "./StageGraph.svelte";
  import {
    cardFacts,
    cardNoData,
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
        data-flip="card-{card.key}"
        data-flip-resize
      >
        <span class="face">
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
              {latencyTop}
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
  /* One card per stage across the console; the strips share one scale, so a stage's shape compares. */
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
  /* Four cards keep a row while each holds a two-way figure and its unit, about 230 px; then they pair up, while
     three stay in one row. */
  @container results (max-width: 960px) {
    .result-cards:has(> :nth-child(4)) {
      grid-template-columns: repeat(2, minmax(0, 1fr));
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
    box-shadow: var(--elev-tile);
    transition: var(--transition-control);
  }
  .card:has(> .strip) {
    grid-template-rows: auto auto minmax(68px, 1fr) auto;
  }
  /* The running card lifts a little on a glow in its hue. A stage starting reaches its card last in its wave: the
     edge eases in two beats after the chip and the glow arrives with it in one step, a single repaint rather than
     a blurred shadow redrawn on every frame of a fade. */
  .card:is(.active, .recovering) {
    --edge: color-mix(in oklab, var(--tone) 55%, var(--border));
    box-shadow:
      var(--elev-tile),
      0 12px 32px -16px color-mix(in oklab, var(--tone) 60%, transparent);
    transition:
      border-color var(--dur-stage) var(--ease-out) calc(2 * var(--beat)),
      box-shadow 0s linear calc(2 * var(--beat));
  }
  @media (prefers-reduced-motion: no-preference) {
    /* A stage that settles lays its facts down one row after another under the figure. */
    .card:is(.complete, .partial, .stopped, .failed) .facts > div {
      animation: row-in 260ms var(--ease-settle) backwards;
      animation-delay: calc(var(--n, 0) * 40ms + var(--beat));
    }
    .facts > div:nth-child(2) {
      --n: 1;
    }
    .facts > div:nth-child(3) {
      --n: 2;
    }
    .facts > div:nth-child(4) {
      --n: 3;
    }
    .facts > div:nth-child(5) {
      --n: 4;
    }
  }
  .card:is(.pending, .not-run) {
    --edge: var(--border-subtle);
    border-top-color: color-mix(in oklab, var(--tone) 45%, transparent);
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
  /* A card that has measured nothing yet shows its facts' names with dashes, so the page is laid out from Start
     and every "—" marks where a value arrives; the names keep their soft colour, which reads at AA. */
  .facts.unknown dd {
    color: var(--text-soft);
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
  /* A phone stacks the cards in stage order, each whole from Start, so nothing moves as the stages run. Each is
     compact, so all of them fit under the dial: the figure beside the name, a short strip, and the facts as one
     row of columns, every one still there. */
  @container results (max-width: 520px) {
    .result-cards {
      grid-template-columns: minmax(0, 1fr);
      gap: var(--space-2);
    }
    .card {
      gap: 2px;
      padding: 8px var(--space-3) 10px;
    }
    /* The figure stands at the right across the name's line and its detail's. */
    .card {
      grid-template-columns: minmax(0, 1fr) auto;
      grid-template-areas: "name value" "line value" "strip strip" "facts facts";
    }
    .card:has(> .strip) {
      grid-template-rows: 20px 16px 36px auto;
    }
    .face {
      display: contents;
    }
    .name {
      grid-area: name;
    }
    .headline {
      grid-area: value;
      align-self: center;
      font-size: 22px;
    }
    .line {
      grid-area: line;
    }
    .strip {
      grid-area: strip;
    }
    .strip {
      min-height: 36px;
      max-height: 36px;
    }
    .facts {
      grid-area: facts;
      grid-template-columns: none;
      grid-auto-columns: max-content;
      grid-auto-flow: column;
      justify-content: space-between;
      column-gap: var(--space-3);
      margin-top: 2px;
    }
    .facts > div {
      flex-direction: column;
      align-items: flex-start;
      gap: 3px;
      height: auto;
      padding-top: 4px;
    }
    .facts > div + div {
      border-top: 0;
    }
    .facts dt {
      font-size: var(--type-xs);
    }
    .wire .delta {
      display: none;
    }
  }
</style>
