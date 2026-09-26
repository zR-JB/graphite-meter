<script lang="ts">
  import Icon from "./Icon.svelte";
  import Disclosure from "./Disclosure.svelte";
  import type { SummaryCard, SummaryRow } from "../presentation/resultSummary";
  import type { MultiServerResult } from "../runner/measure";
  import ServerScope from "./ServerScope.svelte";
  import { JARGON, STAGE, STATUS } from "../presentation/vocabulary";

  let {
    cards,
    details,
    scope = "",
    onscope,
  }: {
    cards: SummaryCard[];
    details?: MultiServerResult | null;
    scope?: string;
    onscope?: (id: string) => void;
  } = $props();
  // One state for every card, so the row of cards opens and closes together.
  let open = $state(false);
</script>

{#snippet line({ label, value, stage, note }: SummaryRow)}
  <span class="line">
    <span class="line-label">
      {#if stage}<span class="line-icon" data-tone={stage}
          ><Icon name={STAGE[stage].icon} /></span
        >{#if label !== STAGE[stage].short}<span class="sr-only"
            >{STAGE[stage].short}</span
          >{/if}{/if}{label}
    </span>
    <span class="line-value">{value}</span>
    {#if note}<span class="line-note">{note}</span>{/if}
  </span>
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
        aggregate="Combined"
        label="Result measurements"
      />
    </div>
  {/if}
  <div class="result-cards">
    {#each cards as card (card.key)}
      <Disclosure class="surface result-card enter {card.status}" bind:open>
        {#snippet summary()}
          <span class="head" data-tone={card.key}>
            <span class="tone-icon" aria-hidden="true"
              ><Icon name={card.icon} /></span
            >
            <span class="label">{card.label}</span>
            {#if card.status === "partial" || card.status === "failed"}
              <span class="badge" data-tone="err">{STATUS[card.status]}</span>
            {/if}
          </span>
          {#key scope}
            <span class="readout enter">
              <span
                class="val"
                aria-hidden={card.accessible ? "true" : undefined}
              >
                <span class="num">{card.num}</span>
                <span class="unit">{card.unit}</span>
              </span>
              {#if card.accessible}<span class="sr-only">{card.accessible}</span
                >{/if}
              {#each card.rows as row (row.label + row.stage)}
                {@render line(row)}
              {/each}
            </span>
          {/key}
        {/snippet}
        <div class="readout details">
          {#each card.details as row (row.label)}
            {@render line(row)}
          {:else}
            <p class="hint">No further measurements.</p>
          {/each}
        </div>
        {#if card.key === "latency"}
          <p class="hint">{JARGON.jitter}</p>
          {#if card.rows.length > 1}<p class="hint">
              {JARGON.addedLatency}
            </p>{/if}
        {/if}
      </Disclosure>
    {/each}
  </div>
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
  @container results (max-width: 330px) {
    .result-cards {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .result-cards > :global(.result-card) {
    --disclosure-pad: var(--space-3);
    transition: var(--transition-control);
  }
  .result-cards > :global(.active) {
    border-color: var(--brand-line);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }
  .head .badge {
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
  .details {
    padding-top: var(--space-2);
    border-top: 1px solid var(--border-subtle);
  }
  .val {
    display: flex;
    align-items: baseline;
    gap: 6px;
    margin: 2px 0 var(--space-1);
  }
  .num {
    font: var(--w-strong) var(--type-xl) / 1 var(--font-display);
    font-variant-numeric: tabular-nums;
    letter-spacing: var(--track-tight);
  }
  .unit {
    color: var(--text-soft);
    font: var(--w-heavy) var(--type-xs) var(--font-mono);
  }
  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 0 var(--space-2);
    min-width: 0;
  }
  .line-label {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    color: var(--text-soft);
    font-size: var(--type-xs);
  }
  .line-icon {
    display: inline-grid;
    color: var(--tone);
  }
  .line-icon :global(svg) {
    width: 12px;
    height: 12px;
  }
  .line-note {
    flex-basis: 100%;
    color: var(--text-soft);
    font-size: var(--type-xs);
  }
  .line-value {
    margin-left: auto;
    font: var(--w-strong) var(--type-sm) var(--font-mono);
    font-variant-numeric: tabular-nums;
    text-align: end;
  }
</style>
