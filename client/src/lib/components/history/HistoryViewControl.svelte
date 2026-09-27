<script lang="ts">
  import Icon from "../Icon.svelte";
  import { tooltip } from "../../actions/tooltip";
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
    onColumnsChange: (columns: HistoryColumn[]) => void;
    onSortChange: (sort: HistorySort, descending: boolean) => void;
  }

  let { columns, sort, descending, onColumnsChange, onSortChange }: Props =
    $props();
  let open = $state(false);
  const popoverId = $props.id();

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

<div class="view-control">
  <button
    class="btn view-trigger"
    type="button"
    aria-label="Choose columns and sort order"
    aria-haspopup="dialog"
    aria-expanded={open}
    popovertarget={popoverId}
    style:anchor-name={`--${popoverId}`}
    {@attach tooltip(() => "Columns and sort order")}
  >
    <span class="layout-icon"><Icon name="columns" /></span>
    <strong>Columns</strong>
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
    <h3>
      <span {@attach tooltip(() => "Columns\nDate is always shown")}
        >Columns</span
      >
    </h3>
    <div class="menu">
      {#each HISTORY_COLUMNS as column}
        <button
          type="button"
          role="checkbox"
          aria-checked={columns.includes(column)}
          disabled={columns.includes(column) && columns.length === 1}
          onclick={() => toggleColumn(column)}
        >
          <span
            >{#if columns.includes(column)}<Icon name="check" />{/if}</span
          >
          <span>{HISTORY_SORT_LABEL[column]}</span>
        </button>
      {/each}
    </div>
    <h3>
      <span
        {@attach tooltip(() => "Sort by\nResults missing the value stay last")}
        >Sort by</span
      >
    </h3>
    <div class="menu" role="radiogroup" aria-label="Sort by">
      {#each HISTORY_SORTS as option}
        <button
          type="button"
          role="radio"
          aria-checked={sort === option}
          onclick={() => chooseSort(option)}
        >
          <span
            >{#if sort === option}<Icon name="check" />{/if}</span
          >
          <span>{HISTORY_SORT_LABEL[option]}</span>
        </button>
      {/each}
    </div>
    <div
      class="segmented"
      role="group"
      aria-label={`Order for ${HISTORY_SORT_LABEL[sort]}`}
    >
      {#each directionOptions as option (option.label)}
        <button
          type="button"
          aria-pressed={descending === option.descending}
          onclick={() => onSortChange(sort, option.descending)}
          >{option.label}</button
        >
      {/each}
    </div>
  </div>
</div>

<style>
  .layout-icon {
    color: var(--brand-strong);
  }
  .layout-icon :global(svg) {
    width: 15px;
    height: 15px;
  }
  .view-popover {
    display: grid;
    gap: var(--space-1);
    width: min(240px, calc(100vw - 2 * var(--space-4)));
    max-height: min(80dvh, 520px);
    padding: var(--space-2);
    overflow-y: auto;
  }
  h3 {
    padding: var(--space-2) var(--space-2) 0;
  }
  h3 + .menu {
    padding: 0;
  }
  .menu > button {
    min-height: 32px;
  }
  .segmented {
    margin-top: var(--space-1);
  }
  .segmented > button {
    flex: 1;
  }
</style>
