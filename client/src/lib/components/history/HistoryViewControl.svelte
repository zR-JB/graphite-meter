<script lang="ts">
  import { onMount, tick } from "svelte";
  import { tooltip } from "../../actions/tooltip";
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
  };

  const directionOptions = $derived.by(() => {
    if (sort === "date")
      return [
        { descending: true, label: "Newest first", symbol: "↓" },
        { descending: false, label: "Oldest first", symbol: "↑" },
      ];
    if (sort === "idle" || sort === "loaded")
      return [
        { descending: false, label: "Lowest first", symbol: "↑" },
        { descending: true, label: "Highest first", symbol: "↓" },
      ];
    return [
      { descending: true, label: "Fastest first", symbol: "↓" },
      { descending: false, label: "Slowest first", symbol: "↑" },
    ];
  });

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

<div class="view-control" class:compact>
  <button
    bind:this={trigger}
    class="view-trigger"
    type="button"
    aria-label={compact
      ? "Choose history view and sort"
      : "Choose visible columns"}
    aria-haspopup="dialog"
    aria-expanded={open}
    use:tooltip={compact
      ? "Choose columns and sort order"
      : "Choose visible columns"}
    onclick={toggle}
  >
    <span class="layout-icon">{@html ICON.columns}</span>
    <strong>{compact ? "View & sort" : "Columns"}</strong>
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
        <div
          class="direction-options"
          role="group"
          aria-label={`Order for ${HISTORY_SORT_LABEL[sort]}`}
        >
          {#each directionOptions as option (option.label)}
            <button
              type="button"
              aria-label={`${HISTORY_SORT_LABEL[sort]}: ${option.label}`}
              aria-pressed={descending === option.descending}
              onclick={() => onSortChange(sort, option.descending)}
            >
              <span aria-hidden="true">{option.symbol}</span>
              {option.label}
            </button>
          {/each}
        </div>
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
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-height: 34px;
    padding: 0 10px;
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
  .layout-icon,
  .check {
    display: grid;
    place-items: center;
  }
  .layout-icon :global(svg),
  .check :global(svg) {
    width: 15px;
    height: 15px;
  }
  .layout-icon,
  .check {
    color: var(--brand-strong);
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
    transform-origin: top right;
  }
  .view-control.compact .view-popover {
    width: min(280px, calc(100vw - 32px));
    max-height: min(60dvh, 430px);
    overflow-y: auto;
    overscroll-behavior: contain;
  }
  .group-head {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    padding: 7px 8px 5px;
    color: var(--text-muted);
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
  .view-control.compact .options {
    grid-template-columns: minmax(0, 1fr);
  }
  .options button,
  .direction-options button {
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
  .direction-options button:hover,
  .direction-options button:focus-visible,
  .direction-options button[aria-pressed="true"] {
    background: var(--brand-soft);
    color: var(--text);
  }
  .options button:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .direction-options {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 2px;
    margin-top: 3px;
    padding-top: 3px;
    border-top: 1px solid var(--border-subtle);
  }
  .direction-options button {
    width: 100%;
  }
  .direction-options button > span {
    color: var(--brand-strong);
    font: 750 15px var(--font-mono);
    text-align: center;
  }
  @media (prefers-reduced-motion: no-preference) {
    .view-popover {
      animation: reveal-view-menu var(--dur-hover) var(--ease-out) both;
    }
    @keyframes reveal-view-menu {
      from {
        opacity: 0;
        transform: translateY(-3px) scale(0.985);
      }
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .view-trigger,
    .view-popover {
      animation: none;
      transition: none;
    }
  }
  @media (max-width: 420px) {
    .group-head {
      align-items: flex-start;
      flex-direction: column;
      gap: 2px;
    }
  }
</style>
