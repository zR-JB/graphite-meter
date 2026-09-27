<script lang="ts">
  import Icon from "./Icon.svelte";
  import {
    cardLine,
    cardTip,
    type SummaryCard,
  } from "../presentation/resultSummary";
  import type { MultiServerResult } from "../runner/measure";
  import ServerScope from "./ServerScope.svelte";
  import { term, tipGroup, tooltip } from "../actions/tooltip";
  import { STAGE, STATUS, STATUS_TONE } from "../presentation/vocabulary";

  let {
    cards,
    fade = 1,
    reserve = false,
    details,
    issues = [],
    scope = "",
    onscope,
    locked = false,
  }: {
    cards: SummaryCard[];
    fade?: number;
    reserve?: boolean;
    details?: MultiServerResult | null;
    issues?: { server: string; stages: string; reason: string }[];
    scope?: string;
    onscope?: (id: string) => void;
    locked?: boolean;
  } = $props();
  const uid = $props.id();
</script>

<div class="result-summary" style:--cards={Math.min(4, cards.length)}>
  {#if details && details.selection.length > 1 && onscope}
    <div class="summary-scope">
      {#if details.participants.length < details.selection.length}<span
          class="hint"
          >{details.participants.length} of {details.selection.length} servers</span
        >{/if}
      <ServerScope
        servers={details.selection}
        value={scope}
        onchange={onscope}
        disabled={locked}
        disabledIds={details.selection
          .filter(
            ({ id }) => !details.servers.some(({ server }) => server.id === id),
          )
          .map(({ id }) => id)}
        aggregate="Combined"
        label="Result measurements"
      />
    </div>
  {/if}
  <div class="chips" class:reserve data-tip-group {@attach tipGroup}>
    {#each cards as card (card.key)}
      {@const quiet = card.status === "pending" || card.status === "not-run"}
      {@const tone = STATUS_TONE[card.status as keyof typeof STATUS_TONE]}
      {@const line = cardLine(card)}
      <article
        class="chip {card.status}"
        data-tone={card.key}
        style:--fade={fade}
      >
        {#if card.trace}
          {@const trace = card.trace}
          <span class="trace" aria-hidden="true">
            <svg viewBox="0 0 100 32" preserveAspectRatio="none">
              <defs>
                <linearGradient
                  id="{uid}-{card.key}"
                  x1="0"
                  y1="0"
                  x2="0"
                  y2="1"
                >
                  <stop
                    offset="0"
                    stop-color="var(--tone)"
                    stop-opacity="0.34"
                  />
                  <stop offset="1" stop-color="var(--tone)" stop-opacity="0" />
                </linearGradient>
              </defs>
              <path class="area" d={trace.area} fill="url(#{uid}-{card.key})" />
              <path class="line" d={trace.line} />
            </svg>
            {#if card.status === "active"}<span
                class="head"
                style:left="{trace.head.x * 100}%"
                style:top="{trace.head.y * 100}%"
              ></span>{/if}
          </span>
        {/if}
        <span class="face" {@attach tooltip(() => cardTip(card))}>
          <span class="name">
            <Icon name={card.icon} />
            {card.label}
            {#if tone}<span class="status"
                >{#if tone !== "neutral"}<span
                    class="status-dot"
                    data-tone={tone}
                  ></span>{/if}{STATUS[
                  card.status as keyof typeof STATUS
                ]}</span
              >{/if}
          </span>
          {#key scope}
            <span
              class="val enter"
              class:quiet
              aria-hidden={card.accessible ? "true" : undefined}
            >
              <span class="num">{card.num}</span>
              {#if card.unit}<span class="unit">{card.unit}</span>{/if}
            </span>
          {/key}
        </span>
        {#if line && !quiet}
          <span class="facts">
            {#if line.label}<span
                class="label"
                {@attach line.tip ? term(() => line.tip!) : null}
                >{line.label}</span
              >{/if}
            {#each line.facts as fact, index (index)}
              <span class="fact"
                >{#if fact.stage}<span class="fact-icon" data-tone={fact.stage}
                    ><Icon name={STAGE[fact.stage].icon} /></span
                  ><span class="sr-only">{STAGE[fact.stage].short}</span
                  >{/if}{fact.value}</span
              >
            {/each}
            {#if line.mark}
              {@const mark = line.mark}
              <span class="mark" {@attach term(() => mark.tip)}
                >{mark.text}</span
              >
            {/if}
          </span>
        {/if}
        {#if card.accessible}<span class="sr-only">{card.accessible}</span>{/if}
      </article>
    {/each}
  </div>
  {#if issues.length}
    {@const attributed = (details?.selection.length ?? 1) > 1 && !scope}
    <dl class="issues enter" class:attributed aria-label="Issues">
      {#each issues as issue, index (index)}
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
    gap: var(--space-2);
    width: 100%;
    max-width: calc(var(--cards) * 175px);
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
  .chips {
    display: grid;
    grid-template-columns: repeat(var(--cards), minmax(0, 1fr));
    gap: var(--space-2);
  }
  @container results (max-width: 480px) {
    .chips {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
  .chip {
    --edge: var(--border);
    position: relative;
    isolation: isolate;
    display: grid;
    grid-template-rows: 14px 26px 15px;
    align-content: start;
    row-gap: 2px;
    height: 74px;
    padding: var(--space-2) var(--space-3) 0;
    overflow: hidden;
    border: var(--hairline) solid var(--edge);
    border-radius: var(--r-chrome);
    background:
      linear-gradient(transparent 30%, var(--tone-wash)), var(--surface-1);
    box-shadow: var(--elev-tile);
    transition: var(--transition-control);
  }
  .reserve .chip {
    height: 82px;
  }
  .chip.active {
    --edge: color-mix(in srgb, var(--tone) 70%, var(--border));
    box-shadow:
      var(--elev-tile),
      0 0 0 3px var(--tone-wash);
  }
  .chip:is(.pending, .not-run) {
    --edge: var(--border-subtle);
  }
  .trace {
    position: absolute;
    inset: auto 0 0;
    z-index: -1;
    height: 14px;
    opacity: var(--fade);
  }
  .trace svg {
    display: block;
    width: 100%;
    height: 100%;
  }
  .line {
    fill: none;
    stroke: var(--tone);
    stroke-width: 1.5;
    stroke-linejoin: round;
    vector-effect: non-scaling-stroke;
  }
  .head {
    position: absolute;
    width: 6px;
    height: 6px;
    border-radius: var(--r-full);
    background: var(--tone);
    box-shadow: 0 0 0 3px var(--tone-wash);
    translate: -50% -50%;
  }
  .face {
    display: grid;
    grid-row: 1 / 3;
    grid-template-rows: subgrid;
    min-width: 0;
  }
  .name {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
    color: var(--tone-ink);
    font: var(--w-strong) var(--type-sm) / 14px var(--font-sans);
    white-space: nowrap;
  }
  .name > :global(svg) {
    flex: none;
    width: 12px;
    height: 12px;
    color: var(--tone);
  }
  .status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin-left: auto;
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .val,
  .facts,
  .status {
    opacity: var(--fade);
  }
  .val {
    display: flex;
    align-items: baseline;
    gap: 5px;
    min-width: 0;
    white-space: nowrap;
  }
  .num {
    font: var(--w-strong) var(--type-xl) / 26px var(--font-display);
    letter-spacing: var(--track-tight);
  }
  .quiet .num {
    color: var(--text-soft);
  }
  .unit {
    color: var(--text-soft);
    font: var(--w-heavy) var(--type-xs) var(--font-mono);
  }
  .facts {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    overflow: hidden;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-xs) / 15px var(--font-sans);
    white-space: nowrap;
  }
  .label {
    color: var(--text-soft);
  }
  .fact {
    display: inline-flex;
    align-items: center;
    gap: 1px;
  }
  .fact-icon {
    display: inline-grid;
    color: var(--tone);
  }
  .fact-icon :global(svg) {
    width: 10px;
    height: 10px;
  }
  .mark {
    color: var(--tone-ink);
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
  .reason {
    color: var(--text-soft);
  }
</style>
