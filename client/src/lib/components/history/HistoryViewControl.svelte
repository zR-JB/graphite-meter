<script lang="ts">
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
  const popoverId = $props.id();

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
</script>

<div class="view-control" class:compact>
  <button
    class="btn view-trigger"
    type="button"
    aria-label={compact
      ? "Choose history view and sort"
      : "Choose visible columns"}
    aria-haspopup="dialog"
    aria-expanded={open}
    popovertarget={popoverId}
    style:anchor-name={`--${popoverId}`}
    use:tooltip={compact
      ? "Choose columns and sort order"
      : "Choose visible columns"}
  >
    <span class="layout-icon">{@html ICON.columns}</span>
    <strong>{compact ? "View & sort" : "Columns"}</strong>
  </button>
  <div
    id={popoverId}
    class="float popover align-end view-popover"
    popover="auto"
    role="dialog"
    tabindex="-1"
    aria-label="History view options"
    style:position-anchor={`--${popoverId}`}
    ontoggle={(event) => {
      open = event.newState === "open";
      if (open)
        event.currentTarget.querySelector<HTMLElement>("button")?.focus();
    }}
  >
    <div class="caps group-head">
      <span>Visible columns</span><small>Date is always shown</small>
    </div>
    <div class="options menu">
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
      <div class="caps group-head sort-head">
        <span>Sort cards</span><small>Missing values stay last</small>
      </div>
      <div class="options menu">
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
        class="direction-options menu"
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
</div>

<style>
  .layout-icon,
  .check {
    color: var(--brand-strong);
  }
  .layout-icon :global(svg) {
    width: 15px;
    height: 15px;
  }
  .view-popover {
    width: 260px;
    padding: var(--space-1);
  }
  .compact .view-popover {
    width: 280px;
    max-height: min(60dvh, 430px);
  }
  .group-head {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: 2px var(--space-2);
    padding: 7px var(--space-2) 5px;
    color: var(--text-muted);
  }
  .group-head small {
    font: inherit;
    letter-spacing: 0;
    text-transform: none;
  }
  .sort-head {
    margin-top: 5px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
  }
  .menu {
    padding: 0;
  }
  .options {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
  .compact .options {
    grid-template-columns: minmax(0, 1fr);
  }
  .menu > button {
    grid-template-columns: 17px minmax(0, 1fr);
    min-height: 33px;
  }
  .check :global(svg) {
    width: 15px;
    height: 15px;
  }
  .direction-options {
    grid-template-columns: repeat(2, minmax(0, 1fr));
    margin-top: 3px;
    padding-top: 3px;
    border-top: 1px solid var(--border-subtle);
  }
  .direction-options button > span {
    font: var(--w-heavy) 15px var(--font-mono);
    text-align: center;
  }
</style>
