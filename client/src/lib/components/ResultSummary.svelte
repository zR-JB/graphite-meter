<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { SummaryCard, SummaryRow } from "../presentation/resultSummary";
  import type { MultiServerResult } from "../runner/measure";
  import ServerScope from "./ServerScope.svelte";
  import { tipGroup, tooltip } from "../actions/tooltip";
  import { STAGE, STATUS, STATUS_TONE } from "../presentation/vocabulary";

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

{#snippet line({ label, value, stage, tip }: SummaryRow)}
  <span class="line" {@attach tip ? tooltip(() => tip) : null}>
    <span class="line-label">
      {#if stage}<span class="line-icon" data-tone={stage}
          ><Icon name={STAGE[stage].icon} /></span
        >{#if label !== STAGE[stage].short}<span class="sr-only"
            >{STAGE[stage].short}</span
          >{/if}{/if}{label}
    </span>
    <span class="line-value">{value}</span>
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
        disabled={locked}
        aggregate="Combined"
        label="Result measurements"
      />
    </div>
  {/if}
  <div class="result-cards" data-tip-group {@attach tipGroup}>
    {#each cards as card (card.key)}
      <article class="surface result-card enter {card.status}">
        <span class="headline" {@attach tooltip(() => card.tip)}>
          <span class="head" data-tone={card.key}>
            <span class="tone-icon" aria-hidden="true"
              ><Icon name={card.icon} /></span
            >
            <span class="label">{card.label}</span>
            {#if card.status === "partial" || card.status === "failed" || card.status === "stopped"}
              <span class="badge" data-tone={STATUS_TONE[card.status]}
                >{STATUS[card.status]}</span
              >
            {/if}
          </span>
          {#key scope}
            <span
              class="val enter"
              aria-hidden={card.accessible ? "true" : undefined}
            >
              <span class="num">{card.num}</span>
              {#if card.unit}<span class="unit">{card.unit}</span>{/if}
            </span>
          {/key}
        </span>
        {#if card.accessible}<span class="sr-only">{card.accessible}</span>{/if}
        {#key scope}
          <span class="readout enter">
            {#if card.wire}
              {@const wire = card.wire}
              <span class="wire" {@attach tooltip(() => wire.tip)}
                ><span>{wire.value}</span><span class="wire-tag"
                  >wire {wire.overhead}</span
                ></span
              >
            {/if}
            {#if card.rows.length}
              <span class="rows">
                {#each card.rows as row (row.label + row.stage)}
                  {@render line(row)}
                {/each}
              </span>
            {/if}
          </span>
        {/key}
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
  @container results (max-width: 330px) {
    .result-cards {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .result-card {
    display: grid;
    align-content: start;
    gap: var(--space-1);
    min-width: 0;
    padding: var(--space-3);
    transition: var(--transition-control);
  }
  .result-card.active {
    border-color: var(--brand-line);
  }
  .headline {
    display: grid;
    gap: var(--space-2);
    justify-self: start;
    min-width: 0;
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }
  .head .badge {
    margin-left: var(--space-1);
  }
  .label {
    font-size: var(--type-sm);
    font-weight: var(--w-heavy);
  }
  .readout {
    display: grid;
    gap: var(--space-1);
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
  .unit {
    color: var(--text-soft);
    font: var(--w-heavy) var(--type-xs) var(--font-mono);
  }
  /* Value and overhead wrap as whole units, never mid-unit. */
  .wire {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0 var(--space-2);
    justify-self: start;
    color: var(--text-muted);
    font: var(--w-strong) var(--type-xs) var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .wire > span {
    white-space: nowrap;
  }
  .wire-tag {
    color: var(--text-soft);
    font-weight: var(--w-normal);
  }
  .rows {
    display: grid;
    gap: 3px;
    margin-top: var(--space-1);
  }
  .line {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-2);
    min-width: 0;
  }
  @media (pointer: coarse) {
    .line {
      min-height: 24px;
      align-items: center;
    }
  }
  .line-label {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    color: var(--text-soft);
    font-size: var(--type-xs);
    transition: color var(--dur-hover) var(--ease-out);
  }
  @media (hover: hover) {
    .line:hover .line-label,
    .headline:hover .unit,
    .wire:hover .wire-tag {
      color: var(--text);
    }
  }
  .line-icon {
    display: inline-grid;
    color: var(--tone);
  }
  .line-icon :global(svg) {
    width: 12px;
    height: 12px;
  }
  .line-value {
    font: var(--w-strong) var(--type-sm) var(--font-mono);
    font-variant-numeric: tabular-nums;
    text-align: end;
  }
</style>
