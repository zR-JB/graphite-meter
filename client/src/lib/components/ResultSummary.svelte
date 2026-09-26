<script lang="ts">
  import { tooltip, JARGON } from "../actions/tooltip";
  import type { SummaryCard } from "../presentation/resultSummary";
  import type { MultiServerResult } from "../servers/measurement";
  import ServerScope from "./ServerScope.svelte";

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
  const QUALITY_TONE = { high: "ok", medium: "warn", low: "err" } as const;
</script>

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
    {#each cards as card, index (card.key)}
      <article
        class="surface result-card enter"
        data-tone={card.key}
        style:--i={index}
      >
        <header>
          <span class="tone-icon" aria-hidden="true">{@html card.icon}</span>
          {#if card.key === "latency"}
            <span class="label term" use:tooltip={JARGON.latency}
              >{card.label}</span
            >
          {:else}
            <span class="label">{card.label}</span>
          {/if}
          {#if card.quality}
            {@const pct = `Measurement stability: ${Math.round(card.quality.pct)}%`}
            <span
              class="badge term"
              data-tone={QUALITY_TONE[card.quality.band]}
              use:tooltip={pct}
              >{card.quality.band}<span class="sr-only">, {pct}</span></span
            >
          {:else if card.status !== "complete"}
            <span class="badge" data-tone="err"
              >{card.status === "partial" ? "Partial" : "Failed"}</span
            >
          {/if}
        </header>
        {#key scope}
          <div class="readout enter">
            <p class="val">
              <span class="num">{card.num}</span>
              <span class="unit">{card.unit}</span>
            </p>
            {#if card.jitter !== null}
              <p class="line">
                <strong>{card.jitter} <small>ms</small></strong>
                <span class="term" use:tooltip={JARGON.jitter}>jitter</span>
              </p>
            {/if}
            {#if card.wire}
              <p class="line">
                <strong>{card.wire.num}</strong>
                <span class="term" use:tooltip={card.wire.tooltip}
                  >wire{card.wire.pct ? ` ${card.wire.pct}` : ""}</span
                >
              </p>
            {/if}
            {#if card.detail}<p class="detail">{card.detail}</p>{/if}
          </div>
        {/key}
      </article>
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
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: var(--space-2);
  }
  @container results (max-width: 452px) {
    .result-cards {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .result-card:last-child:nth-child(odd) {
      grid-column: 1 / -1;
    }
  }
  @container results (max-width: 301px) {
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
    transition-delay: calc(var(--i) * 40ms);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  header .badge {
    margin-left: auto;
  }
  .label {
    font-size: var(--type-sm);
    font-weight: var(--w-heavy);
  }
  .readout {
    display: grid;
    gap: 5px;
    min-width: 0;
  }
  .val,
  .line {
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
  .line {
    font: var(--type-sm) var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .line strong {
    color: var(--brand-strong);
  }
  .line small,
  .line .term {
    color: var(--text-soft);
    font-size: var(--type-2xs);
  }
  .detail {
    color: var(--text-soft);
    font: var(--type-xs) var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
</style>
