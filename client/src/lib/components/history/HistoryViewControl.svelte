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
    aria-label="Choose visible columns"
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
  @media (max-width: 560px) {
    .view-popover {
      right: auto;
      left: 0;
    }
  }
</style>
