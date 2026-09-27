<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { SummaryCard, SummaryRow } from "../presentation/resultSummary";
  import type { MultiServerResult } from "../runner/measure";
  import ServerScope from "./ServerScope.svelte";
  import { tipGroup, tooltip } from "../actions/tooltip";
  import {
    MISSING,
    STAGE,
    STATUS,
    STATUS_TONE,
  } from "../presentation/vocabulary";

  let {
    cards,
    details,
    issues = [],
    scope = "",
    onscope,
    locked = false,
  }: {
    cards: SummaryCard[];
    details?: MultiServerResult | null;
    issues?: { server: string; text: string }[];
    scope?: string;
    onscope?: (id: string) => void;
    locked?: boolean;
  } = $props();
</script>

<!-- Facts read "value term"; the term carries the explainer, a run of stage icons shares one term. -->
{#snippet facts(rows: SummaryRow[])}
  <span class="facts"
    ><span class="facts-line">
      {#each rows as row, i (row.label + row.stage)}
        {@const lane = row.stage && row.label === STAGE[row.stage].short}
        {@const joined = row.stage && rows[i + 1]?.label === row.label}
        <span class="fact">
          {#if row.stage}<span class="fact-icon" data-tone={row.stage}
              ><Icon name={STAGE[row.stage].icon} /></span
            ><span class="sr-only">{STAGE[row.stage].short}</span>{/if}
          <span class="fact-value">{row.value}</span>
          {#if !lane && !joined}<span
              class="fact-term"
              {@attach row.tip ? tooltip(() => row.tip!) : null}
              >{row.label.toLowerCase()}</span
            >{/if}
        </span>
      {/each}
    </span></span
  >
{/snippet}

<div class="result-summary" style:--cards={Math.min(4, cards.length)}>
  {#if details && details.selection.length > 1 && onscope}
    <div class="summary-scope">
      {#if details.participants.length < details.selection.length}<span
          class="caps"
          >{details.participants.length} of {details.selection.length} servers</span
        >{/if}
      <ServerScope
        servers={details.selection}
        value={scope}
        onchange={onscope}
        disabled={locked}
        aggregate="Combined"
        label="Result measurements"
      />
    </div>
  {/if}
  <div class="result-cards" data-tip-group {@attach tipGroup}>
    {#each cards as card (card.key)}
      {@const stability = card.rows.find((row) => row.label === "Stability")}
      <article class="surface result-card enter {card.status}">
        <span class="head" data-tone={card.key}>
          <span class="tone-icon" aria-hidden="true"
            ><Icon name={card.icon} /></span
          >
          <span class="label" {@attach tooltip(() => card.tip)}
            >{card.label}</span
          >
          {#if card.status === "partial" || card.status === "failed" || card.status === "stopped"}
            <span class="badge" data-tone={STATUS_TONE[card.status]}
              >{STATUS[card.status]}</span
            >
          {:else if stability && stability.value !== MISSING}
            <span class="stability"
              >{stability.value}
              <span
                class="fact-term"
                {@attach stability.tip ? tooltip(() => stability.tip!) : null}
                >stable</span
              ></span
            >
          {/if}
        </span>
        {#key scope}
          <span class="readout enter">
            <span
              class="val"
              aria-hidden={card.accessible ? "true" : undefined}
            >
              <span class="num">{card.num}</span>
              {#if card.unit}<span class="unit">{card.unit}</span>{/if}
            </span>
            {#if card.wire}
              {@const wire = card.wire}
              <span class="wire"
                >{wire.value}
                <span class="fact-term" {@attach tooltip(() => wire.tip)}
                  >wire {wire.overhead}</span
                ></span
              >
            {/if}
            {@render facts(card.rows.filter((row) => row !== stability))}
          </span>
        {/key}
        {#if card.accessible}<span class="sr-only">{card.accessible}</span>{/if}
      </article>
    {/each}
  </div>
  {#if issues.length}
    <section class="group enter" aria-label="Issues">
      <h3 class="caps">Issues</h3>
      <dl class="kv">
        {#each issues as issue, index (index)}
          <div>
            <dt>{issue.server}</dt>
            <dd>{issue.text}</dd>
          </div>
        {/each}
      </dl>
    </section>
  {/if}
</div>

<style>
  .result-summary {
    display: grid;
    gap: var(--space-2);
    width: 100%;
    max-width: calc(var(--cards) * 280px);
    margin-inline: auto;
    container: results / inline-size;
  }
  .summary-scope {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
  }
  .result-cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: var(--space-2);
  }
  @container results (max-width: 540px) {
    .result-cards {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .result-cards > :global(:last-child:nth-child(odd)) {
      grid-column: 1 / -1;
    }
  }
  @container results (max-width: 480px) {
    .result-cards {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .result-card {
    display: grid;
    align-content: start;
    gap: 6px;
    min-width: 0;
    padding: 10px var(--space-3);
    transition: var(--transition-control);
  }
  .result-card.active {
    border-color: var(--brand-line);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }
  .head .badge,
  .stability {
    margin-left: auto;
  }
  .label {
    font-size: var(--type-sm);
    font-weight: var(--w-heavy);
  }
  .readout {
    display: grid;
    gap: 3px;
    min-width: 0;
  }
  .val {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .num {
    font: var(--w-strong) var(--type-xl) / 1 var(--font-display);
    font-variant-numeric: tabular-nums;
    letter-spacing: var(--track-tight);
  }
  /* A live value changes width as it moves; its unit holds still. */
  .active .num {
    min-width: 5ch;
  }
  .unit {
    color: var(--text-soft);
    font: var(--w-heavy) var(--type-xs) var(--font-mono);
  }
  .wire,
  .facts,
  .stability {
    color: var(--text-muted);
    font: var(--w-strong) var(--type-xs) / 1.5 var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  /* Each fact wraps whole; its middot sits in the gap before it, clipped at a line start. */
  .facts {
    display: block;
    overflow: hidden;
  }
  .facts-line {
    display: flex;
    flex-wrap: wrap;
    margin-left: calc(-1 * var(--space-3));
  }
  .fact {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding-left: var(--space-3);
    white-space: nowrap;
  }
  .fact::before {
    content: "·";
    position: absolute;
    left: calc(var(--space-3) / 2 - 0.5ch);
    color: var(--text-soft);
  }
  .fact-icon {
    display: inline-grid;
    color: var(--tone);
  }
  .fact-icon :global(svg) {
    width: 11px;
    height: 11px;
  }
  .fact-term {
    color: var(--text-soft);
    font-weight: var(--w-normal);
    transition: color var(--dur-hover) var(--ease-out);
  }
  @media (hover: hover) {
    .fact-term:hover,
    .label:hover {
      color: var(--text);
    }
  }
</style>
