<script lang="ts">
  import { onMount, tick } from "svelte";
  import { ICON } from "../../constants";
  import {
    HISTORY_SORT_LABEL,
    HISTORY_SORTS,
    naturalDescending,
    type HistorySort,
  } from "../../history/sort";
  import { HISTORY_COLUMNS, type HistoryColumn } from "../../state/persistence";

  interface Props {
    columns: HistoryColumn[];
    sort: HistorySort;
    descending: boolean;
    compact?: boolean;
    onColumnsChange: (columns: HistoryColumn[]) => void;
    onSortChange: (sort: HistorySort, descending: boolean) => void;
  }

  let {
    columns,
    sort,
    descending,
    compact = false,
    onColumnsChange,
    onSortChange,
  }: Props = $props();
  let open = $state(false);
  let trigger = $state<HTMLButtonElement>();
  let popover = $state<HTMLDivElement>();

  const labels: Record<HistoryColumn, string> = {
    download: "Download",
    upload: "Upload",
    bidirectional: "Bidirectional",
    idle: "Idle latency",
    loaded: "Loaded latency",
    status: "Status",
  };

  async function toggle() {
    open = !open;
    if (!open) return;
    await tick();
    popover?.querySelector<HTMLElement>("button")?.focus();
  }

  function toggleColumn(column: HistoryColumn) {
    if (columns.includes(column)) {
      if (columns.length === 1) return;
      onColumnsChange(columns.filter((candidate) => candidate !== column));
    } else {
      onColumnsChange(
        HISTORY_COLUMNS.filter(
          (candidate) => columns.includes(candidate) || candidate === column,
        ),
      );
    }
  }

  function chooseSort(next: HistorySort) {
    onSortChange(next, naturalDescending(next));
  }

  function close() {
    open = false;
    trigger?.focus();
  }

  onMount(() => {
    const outside = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (!trigger?.contains(target) && !popover?.contains(target))
        open = false;
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  });
</script>

<div class="view-control">
  <button
    bind:this={trigger}
    class="view-trigger"
    type="button"
    aria-haspopup="dialog"
    aria-expanded={open}
    onclick={toggle}
  >
    <span>{@html ICON.columns}</span>
    <strong>{compact ? "View & sort" : "Columns"}</strong>
    <span class="chevron" aria-hidden="true">⌄</span>
  </button>
  {#if open}
    <div
      bind:this={popover}
      class="view-popover"
      role="dialog"
      tabindex="-1"
      aria-label="History view options"
      onkeydown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          close();
        }
      }}
    >
      <div class="group-head">
        <span>Visible columns</span><small>Date is always shown</small>
      </div>
      <div class="options">
        {#each HISTORY_COLUMNS as column}
          <button
            type="button"
            role="checkbox"
            aria-checked={columns.includes(column)}
            disabled={columns.includes(column) && columns.length === 1}
            onclick={() => toggleColumn(column)}
          >
            <span class="check"
              >{#if columns.includes(column)}{@html ICON.check}{/if}</span
            >
            <span>{labels[column]}</span>
          </button>
        {/each}
      </div>
      {#if compact}
        <div class="group-head sort-head">
          <span>Sort cards</span><small>Missing values stay last</small>
        </div>
        <div class="options">
          {#each HISTORY_SORTS as option}
            <button
              type="button"
              role="radio"
              aria-checked={sort === option}
              onclick={() => chooseSort(option)}
            >
              <span class="check"
                >{#if sort === option}{@html ICON.check}{/if}</span
              >
              <span>{HISTORY_SORT_LABEL[option]}</span>
            </button>
          {/each}
        </div>
        <button
          class="direction"
          type="button"
          onclick={() => onSortChange(sort, !descending)}
        >
          <span aria-hidden="true">{descending ? "↓" : "↑"}</span>
          {descending ? "Reverse to ascending" : "Reverse to descending"}
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .view-control {
    position: relative;
  }
  button {
    font-family: var(--font-sans);
  }
  .view-trigger {
    display: grid;
    grid-template-columns: 15px auto 9px;
    align-items: center;
    gap: 7px;
    min-height: 32px;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: inset 0 1px 0 var(--edge-light);
    color: var(--text-muted);
    font-size: var(--type-xs);
    cursor: pointer;
  }
  .view-trigger:hover,
  .view-trigger[aria-expanded="true"] {
    border-color: var(--border-strong);
    color: var(--text);
  }
  .view-trigger > span:first-child,
  .check {
    display: grid;
    place-items: center;
  }
  .view-trigger :global(svg),
  .check :global(svg) {
    width: 14px;
    height: 14px;
  }
  .view-trigger > span:first-child,
  .check {
    color: var(--brand-strong);
  }
  .chevron {
    color: var(--text-soft);
    transform: translateY(-1px);
  }
  .view-popover {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 30;
    width: min(260px, calc(100vw - 32px));
    padding: 6px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--shadow-float);
  }
  .group-head {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    padding: 7px 8px 5px;
    color: var(--text-soft);
    font: 700 9px var(--font-mono);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .group-head small {
    font: inherit;
    letter-spacing: 0;
    text-transform: none;
  }
  .sort-head {
    margin-top: 5px;
    border-top: 1px solid var(--border);
    padding-top: 10px;
  }
  .options {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 2px;
  }
  .options button,
  .direction {
    display: grid;
    grid-template-columns: 17px minmax(0, 1fr);
    align-items: center;
    gap: 7px;
    min-height: 33px;
    padding: 0 7px;
    border: 0;
    border-radius: var(--r-well);
    background: transparent;
    color: var(--text-muted);
    font-size: var(--type-xs);
    text-align: left;
    cursor: pointer;
  }
  .options button:hover,
  .options button:focus-visible,
  .options button[aria-checked="true"],
  .direction:hover {
    background: var(--brand-soft);
    color: var(--text);
  }
  .options button:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .direction {
    width: 100%;
    margin-top: 3px;
    border-top: 1px solid var(--border-subtle);
    border-radius: 0 0 var(--r-well) var(--r-well);
  }
  .direction > span {
    color: var(--brand-strong);
    font: 750 15px var(--font-mono);
    text-align: center;
  }
  @media (max-width: 560px) {
    .view-popover {
      right: auto;
      left: 0;
    }
  }
</style>
