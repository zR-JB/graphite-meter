<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { IconName } from "../presentation/icons";
  import { onMount, tick, untrack } from "svelte";
  import { tooltip } from "../actions/tooltip";
  import { canFocus, hasFocus, activeModal } from "../actions/focus";
  import { createUuid } from "../uuid";
  import {
    announceHistoryChanged,
    HistoryRepository,
    onHistoryChanged,
  } from "../history/repository";
  import {
    formatHistoryRate,
    formatLatency,
    formatRecentCompletion,
    historyOutcome,
    stageStatusLabel,
  } from "../history/format";
  import {
    historyMetrics,
    HISTORY_SORT_LABEL,
    naturalDescending,
    prepareHistorySort,
    sortPreparedHistory,
    type HistorySort,
  } from "../history/sort";
  import { HISTORY_LIMIT, type HistoryRecord } from "../history/types";
  import type { HistoryColumn } from "../state/persistence";
  import { store } from "../state/store.svelte";
  import { bidirectionalResultPresentation } from "../presentation/bidirectionalResult";
  import {
    LATENCY_POPULATION,
    MISSING,
    OUTCOME,
    STAGE,
  } from "../presentation/vocabulary";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import MoreMenu from "./MoreMenu.svelte";
  import HistoryResultDetail from "./history/HistoryResultDetail.svelte";
  import HistoryViewControl from "./history/HistoryViewControl.svelte";

  interface Props {
    selectedId: string | null;
    onNavigate: (id: string | null) => void;
    onClose: () => void;
  }

  let { selectedId, onNavigate, onClose }: Props = $props();
  const repository = new HistoryRepository();
  const changeSource = createUuid();
  let loadState = $state<"loading" | "ready" | "error">("loading");
  let records = $state.raw<HistoryRecord[]>([]);
  let malformedCount = $state(0);
  let selectedState = $state<"ready" | "missing" | "malformed">("missing");
  let sort = $state<HistorySort>("date");
  let descending = $state(true);
  let pages = $state(1);
  let renderedAt = $state(Date.now());
  let workspace = $state<HTMLElement>();
  let detailRegion = $state<HTMLElement>();
  let previousSelectedId: string | null = null;
  let confirm = $state<
    { kind: "delete"; id: string } | { kind: "clear" } | null
  >(null);
  let confirmInvoker = $state<HTMLElement | null>(null);
  let actionError = $state("");
  let announcement = $state("");
  let loadGeneration = 0;

  const columns = $derived(store.historyColumns);
  const ordered = $derived(
    sortPreparedHistory(prepareHistorySort(records), sort, descending),
  );
  const selectedIndex = $derived(
    selectedId ? ordered.findIndex((record) => record.id === selectedId) : -1,
  );
  const visibleCount = $derived(
    Math.max(pages, Math.ceil((selectedIndex + 1) / 50)) * 50,
  );
  const visibleRows = $derived(ordered.slice(0, visibleCount).map(historyRow));
  const selectedRecord = $derived(
    records.find((record) => record.id === selectedId) ?? null,
  );
  const span = $derived.by(() => {
    if (!records.length) return "";
    const times = records.map((record) => record.completedAt);
    const [first, last] = [Math.min(...times), Math.max(...times)].map(
      dateLabel,
    );
    return first === last ? first : `${first} – ${last}`;
  });

  const COLUMN: Record<
    HistoryColumn,
    { short: string; icon: IconName; help?: string }
  > = {
    download: STAGE.download,
    upload: STAGE.upload,
    bidirectional: STAGE.bidirectional,
    idle: {
      short: LATENCY_POPULATION.latency.short,
      icon: STAGE.latency.icon,
    },
    loaded: {
      short: "Loaded",
      icon: STAGE.latency.icon,
      help: "Highest loaded median (p50) across download, upload and bidirectional",
    },
  };

  async function resolveSelection(id: string | null, generation: number) {
    if (!id) {
      selectedState = "missing";
      return;
    }
    if (records.some((record) => record.id === id)) {
      selectedState = "ready";
      return;
    }
    try {
      const entry = await repository.inspect(id);
      if (generation !== loadGeneration || selectedId !== id) return;
      selectedState = entry.status;
      if (entry.status === "ready")
        records = [
          entry.record,
          ...records.filter((record) => record.id !== entry.record.id),
        ].slice(0, HISTORY_LIMIT);
    } catch {
      if (generation === loadGeneration) loadState = "error";
    }
  }

  async function load(showLoading = true) {
    const generation = ++loadGeneration;
    if (showLoading) loadState = "loading";
    actionError = "";
    try {
      const result = await repository.listWithDiagnostics();
      if (generation !== loadGeneration) return;
      records = result.records;
      renderedAt = Date.now();
      malformedCount = result.malformedCount;
      loadState = "ready";
      await resolveSelection(selectedId, generation);
    } catch {
      if (generation === loadGeneration) loadState = "error";
    }
  }

  function setSort(next: HistorySort, nextDescending: boolean) {
    sort = next;
    descending = nextDescending;
    pages = 1;
  }

  function loadMore() {
    pages = visibleCount / 50 + 1;
  }

  function loadMoreWhenVisible(node: HTMLElement) {
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) loadMore();
      },
      { rootMargin: "200px 0px" },
    );
    observer.observe(node);
    return () => observer.disconnect();
  }

  async function confirmAction() {
    const action = confirm;
    const owner = workspace;
    confirm = null;
    if (!action) return;
    actionError = "";
    try {
      if (action.kind === "clear") {
        await repository.clear();
        records = [];
        malformedCount = 0;
        announcement = "History cleared.";
        if (owner?.isConnected && selectedId) onNavigate(null);
        announceHistoryChanged(changeSource);
        confirmInvoker = null;
        await tick();
        const target = workspace?.querySelector<HTMLElement>(".close-history");
        if (!hasFocus() && canFocus(target))
          target.focus({ preventScroll: true });
      } else {
        await repository.delete(action.id);
        records = records.filter((record) => record.id !== action.id);
        announcement = "Result deleted.";
        if (owner?.isConnected && selectedId === action.id) onNavigate(null);
        announceHistoryChanged(changeSource);
      }
    } catch {
      actionError = "History could not be changed. Try again.";
      confirmInvoker = null;
    }
  }

  function requestConfirm(
    action: NonNullable<typeof confirm>,
    invoker: HTMLElement,
  ) {
    confirmInvoker = invoker;
    confirm = action;
  }

  const units = $derived({ base: store.unitBase, kind: store.unitKind });

  function metric(record: HistoryRecord, column: HistoryColumn): string {
    const { stages } = record;
    const value = historyMetrics(record)[column];
    if (column === "loaded")
      return value == null ? MISSING : formatLatency(value);
    if (column === "idle")
      return value == null
        ? stageStatusLabel(stages.latency.status)
        : formatLatency(value);
    if (value != null) return formatHistoryRate(value, units);
    if (column !== "bidirectional")
      return stageStatusLabel(stages[column].status);
    const { survivingDirection } = bidirectionalResultPresentation(
      stages.bidirectional.down?.reportedBytesPerSec,
      stages.bidirectional.up?.reportedBytesPerSec,
    );
    return survivingDirection
      ? `${survivingDirection === "down" ? "Down" : "Up"} only`
      : stageStatusLabel(stages.bidirectional.status);
  }

  function historyRow(record: HistoryRecord) {
    const recent = formatRecentCompletion(record.completedAt, renderedAt);
    const exact = new Date(record.completedAt).toLocaleString(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    });
    const outcome = historyOutcome(record);
    const metrics = columns.map((column) => metric(record, column));
    return {
      record,
      exact,
      primary: recent ?? dateLabel(record.completedAt),
      secondary: recent
        ? dateLabel(record.completedAt)
        : new Date(record.completedAt).toLocaleTimeString(undefined, {
            hour: "2-digit",
            minute: "2-digit",
          }),
      outcome,
      metrics,
      label: [
        `${exact}, ${OUTCOME[outcome].toLowerCase()} result`,
        ...columns.map(
          (column, index) => `${HISTORY_SORT_LABEL[column]} ${metrics[index]}`,
        ),
      ].join(". "),
    };
  }

  function dateLabel(value: number): string {
    return new Date(value).toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
      year: "numeric",
    });
  }

  $effect(() => {
    const id = selectedId;
    untrack(() => {
      if (loadState === "ready") void resolveSelection(id, loadGeneration);
    });
  });

  $effect(() => {
    const id = selectedId;
    const previous = previousSelectedId;
    const region = detailRegion;
    if (id && region && id !== previous) {
      previousSelectedId = id;
      if (!activeModal() && canFocus(region))
        region.focus({ preventScroll: true });
    } else if (!id && previous) {
      previousSelectedId = null;
      const target =
        workspace?.querySelector<HTMLElement>(
          `[data-history-id="${previous}"]`,
        ) ?? workspace?.querySelector<HTMLElement>(".close-history");
      if (!activeModal() && canFocus(target)) target.focus();
    }
  });

  onMount(() => {
    void load();
    const relativeRefresh = window.setInterval(() => {
      if (document.visibilityState === "visible") renderedAt = Date.now();
    }, 60_000);
    const refresh = () => void load(false);
    const refreshWhenVisible = () => {
      if (document.visibilityState === "visible") refresh();
    };
    const stopChanges = onHistoryChanged(refresh, changeSource);
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refreshWhenVisible);
    return () => {
      loadGeneration++;
      stopChanges();
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refreshWhenVisible);
      window.clearInterval(relativeRefresh);
      repository.close();
    };
  });
</script>

<section
  class="history-workspace enter"
  bind:this={workspace}
  aria-labelledby="history-title"
  tabindex="-1"
>
  <header class="surface-head history-head">
    <h1 id="history-title">History</h1>
    {#if records.length}
      <p>
        {records.length}
        {records.length === 1 ? "result" : "results"} · {span}
      </p>
    {/if}
    <div class="head-actions">
      {#if records.length}
        <HistoryViewControl
          {columns}
          {sort}
          {descending}
          onColumnsChange={(next) => store.prefer({ historyColumns: next })}
          onSortChange={setSort}
        />
      {/if}
      {#if records.length || malformedCount}
        <MoreMenu label="History actions" danger>
          {#snippet children(select)}
            <button
              type="button"
              role="menuitem"
              tabindex="-1"
              onclick={() =>
                select((invoker) => requestConfirm({ kind: "clear" }, invoker))}
            >
              <span><Icon name="trash" /></span>
              <span><strong>Clear all saved results</strong></span>
            </button>
          {/snippet}
        </MoreMenu>
      {/if}
      <button
        class="btn btn-icon close-history"
        type="button"
        aria-label="Close History"
        onclick={onClose}
      >
        <Icon name="close" />
      </button>
    </div>
  </header>

  {#if store.historyWarning || actionError || malformedCount || (records.length && !store.savingResults)}
    <div class="notices">
      {#if records.length && !store.savingResults}
        <p class="notice" data-tone="warn">
          <span><strong>Saving is paused.</strong> Saved results remain.</span>
          <button
            class="btn"
            type="button"
            onclick={() => store.prefer({ resultHistoryPreference: "enabled" })}
            >Resume saving</button
          >
        </p>
      {/if}
      {#each [store.historyWarning, actionError].filter(Boolean) as message (message)}
        <p class="notice" data-tone="warn" role="status">{message}</p>
      {/each}
      {#if malformedCount}
        <p class="notice" data-tone="warn" role="status">
          {malformedCount} unsupported or malformed {malformedCount === 1
            ? "record was"
            : "records were"} ignored.
        </p>
      {/if}
    </div>
  {/if}

  {#if loadState === "loading"}
    <div class="empty-state" role="status">
      <span class="empty-icon"><Icon name="history" /></span>
      <h2>Opening History</h2>
    </div>
  {:else if loadState === "error"}
    <div class="empty-state" data-tone="err" role="alert">
      <span class="empty-icon">!</span>
      <h2>History is unavailable</h2>
      <p>The browser could not open its saved results.</p>
      <button class="btn btn-accent" type="button" onclick={() => load()}
        >Retry</button
      >
    </div>
  {:else if records.length === 0}
    <div class="empty-state">
      <span class="empty-icon"><Icon name="history" /></span>
      <h2>No saved results</h2>
      {#if store.savingResults}
        <p>Completed tests appear here automatically.</p>
      {:else}
        <p>
          Saving is paused. Resume it to keep future results on this device.
        </p>
        <button
          class="btn btn-accent"
          type="button"
          onclick={() => store.prefer({ resultHistoryPreference: "enabled" })}
          >Resume saving</button
        >
      {/if}
    </div>
  {:else}
    <div class="workspace-body" class:has-detail={selectedId !== null}>
      <div class="history-list">
        <div class="history-table" style:--metric-columns={columns.length}>
          <div class="column-head" role="group" aria-label="Sort by">
            {#each ["date" as const, ...columns] as column (column)}
              <button
                type="button"
                aria-pressed={sort === column}
                data-order={sort !== column
                  ? undefined
                  : descending
                    ? "descending"
                    : "ascending"}
                use:tooltip={(column !== "date" && COLUMN[column].help) || ""}
                onclick={() =>
                  setSort(
                    column,
                    sort === column ? !descending : naturalDescending(column),
                  )}
              >
                {#if column !== "date"}<span
                    class="head-icon"
                    data-tone={column}><Icon name={COLUMN[column].icon} /></span
                  >{/if}
                <span>{column === "date" ? "Date" : COLUMN[column].short}</span>
                {#if sort === column}<span class="sr-only"
                    >, {descending ? "descending" : "ascending"}</span
                  >{/if}
                <i aria-hidden="true"></i>
              </button>
            {/each}
          </div>
          <ol aria-label="Saved results">
            {#each visibleRows as row (row.record.id)}
              <li>
                <a
                  class="result-row"
                  data-history-id={row.record.id}
                  href={`#/history/${row.record.id}`}
                  aria-current={selectedId === row.record.id
                    ? "true"
                    : undefined}
                  aria-label={row.label}
                  onclick={(event) => {
                    if (
                      event.button !== 0 ||
                      event.metaKey ||
                      event.ctrlKey ||
                      event.shiftKey ||
                      event.altKey
                    )
                      return;
                    event.preventDefault();
                    onNavigate(
                      selectedId === row.record.id ? null : row.record.id,
                    );
                  }}
                >
                  <span class="date-cell">
                    <time
                      datetime={new Date(row.record.completedAt).toISOString()}
                      title={row.exact}
                    >
                      <strong>{row.primary}</strong>
                      <small>{row.secondary}</small>
                    </time>
                    {#if row.outcome !== "complete"}<span
                        class="badge"
                        data-tone="warn">{OUTCOME[row.outcome]}</span
                      >{/if}
                  </span>
                  {#each columns as column, index (column)}
                    <span class="metric-cell" data-tone={column}>
                      <small
                        ><span class="head-icon"
                          ><Icon name={COLUMN[column].icon} /></span
                        >{COLUMN[column].short}</small
                      >
                      <strong>{row.metrics[index]}</strong>
                    </span>
                  {/each}
                </a>
              </li>
            {/each}
          </ol>
        </div>
        {#if visibleCount < ordered.length}
          <div class="load-more" {@attach loadMoreWhenVisible}>
            <button class="btn" type="button" onclick={loadMore}
              >Load 50 more</button
            >
            <span>{visibleCount} of {ordered.length}</span>
          </div>
        {/if}
      </div>

      {#if selectedRecord}
        {#key selectedRecord.id}
          <div class="detail-pane enter">
            <HistoryResultDetail
              record={selectedRecord}
              onClose={() => onNavigate(null)}
              onDelete={(invoker) =>
                requestConfirm(
                  { kind: "delete", id: selectedRecord.id },
                  invoker,
                )}
              bind:region={detailRegion}
            />
          </div>
        {/key}
      {:else if selectedId && selectedState !== "ready"}
        <div class="detail-pane empty-state" role="status">
          <span class="empty-icon">!</span>
          <h2>
            {selectedState === "malformed"
              ? "Unreadable saved result"
              : "Result no longer available"}
          </h2>
          <p>
            {selectedState === "malformed"
              ? "This record uses an unsupported format or failed validation."
              : "It may have been deleted in another tab."}
          </p>
          <button class="btn" type="button" onclick={() => onNavigate(null)}
            >Back to results</button
          >
        </div>
      {/if}
    </div>
  {/if}

  <p class="sr-only" aria-live="polite">{announcement}</p>
</section>

<ConfirmDialog
  open={confirm !== null}
  id="history-confirm"
  invoker={confirmInvoker}
  title={confirm?.kind === "clear"
    ? "Clear result history?"
    : "Delete this result?"}
  description={confirm?.kind === "clear"
    ? "Permanently remove all saved results from this browser?"
    : "Permanently remove this saved result from this browser?"}
  confirmLabel={confirm?.kind === "clear" ? "Clear history" : "Delete result"}
  onCancel={() => {
    confirm = null;
    confirmInvoker = null;
  }}
  onConfirm={confirmAction}
/>

<style>
  .history-workspace {
    display: flex;
    flex: 1 1 auto;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-raised);
    container: history / inline-size;
  }
  .history-workspace:focus {
    outline: none;
  }
  .history-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-1) var(--space-3);
    padding: 10px var(--space-4);
  }
  h1 {
    font: var(--w-heavy) var(--type-lg) / 1.2 var(--font-display);
    letter-spacing: var(--track-tight);
  }
  .history-head p {
    min-width: 0;
    color: var(--text-muted);
    font: var(--type-xs) var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .head-actions {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-left: auto;
  }
  .notices {
    display: grid;
    gap: var(--space-1);
    padding: var(--space-2) var(--space-4);
    border-bottom: 1px solid var(--border);
  }
  .notice {
    align-items: center;
    justify-content: space-between;
  }
  .workspace-body {
    display: grid;
    flex: 1 1 auto;
    grid-template: minmax(0, 1fr) / minmax(0, 1fr);
    min-height: 0;
  }
  .history-list,
  .detail-pane {
    grid-area: 1 / 1;
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior-y: contain;
  }
  .detail-pane {
    background: var(--surface-1);
  }
  .has-detail .history-list {
    visibility: hidden;
  }
  @container history (min-width: 821px) {
    .has-detail {
      grid-template-columns: minmax(0, 1fr) minmax(380px, 0.8fr);
    }
    .has-detail .history-list {
      visibility: visible;
    }
    .detail-pane {
      grid-area: 1 / 2;
      border-left: 1px solid var(--border-strong);
    }
  }
  .history-list {
    container: history-list / inline-size;
  }
  .history-table {
    display: grid;
    grid-template-columns:
      minmax(150px, 1.25fr)
      repeat(var(--metric-columns), minmax(72px, 1fr));
    padding-inline: var(--space-2);
  }
  .column-head,
  ol,
  li,
  .result-row {
    display: grid;
    grid-column: 1 / -1;
    grid-template-columns: subgrid;
    min-width: 0;
  }
  .column-head {
    position: sticky;
    top: 0;
    z-index: 1;
    margin-inline: calc(-1 * var(--space-2));
    padding-inline: var(--space-2);
    border-bottom: 1px solid var(--border-strong);
    background: var(--sheen), var(--surface-1);
  }
  .column-head button {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 6px;
    width: 100%;
    min-height: 34px;
    padding: 0 10px;
    color: var(--text-muted);
    font: var(--w-heavy) var(--type-2xs) var(--font-mono);
    letter-spacing: var(--track-caps);
    text-transform: uppercase;
    transition: var(--transition-control);
  }
  .column-head > button:first-child {
    justify-content: flex-start;
  }
  @media (hover: hover) {
    .column-head button:hover {
      background: var(--brand-soft);
      color: var(--text);
    }
  }
  .column-head [aria-pressed="true"] {
    color: var(--brand-strong);
  }
  .column-head i {
    flex: none;
    width: 6px;
    height: 6px;
    border: solid currentColor;
    border-width: 0 1.5px 1.5px 0;
    opacity: 0;
    rotate: 45deg;
    transition:
      opacity var(--dur-hover) var(--ease-out),
      rotate var(--dur-hover) var(--ease-out);
  }
  .column-head [data-order] i {
    opacity: 1;
  }
  .column-head [data-order="ascending"] i {
    rotate: 225deg;
  }
  .head-icon {
    display: grid;
    color: var(--tone, var(--text-soft));
  }
  .head-icon :global(svg) {
    width: var(--icon-sm);
    height: var(--icon-sm);
  }
  li {
    border-bottom: 1px solid var(--border-subtle);
  }
  .result-row {
    min-height: 54px;
    border-radius: var(--r-well);
    transition: var(--transition-control);
  }
  @media (hover: hover) {
    .result-row:hover {
      background: var(--surface-2);
    }
  }
  .result-row[aria-current="true"] {
    background: var(--brand-soft);
    box-shadow: inset 2px 0 0 var(--brand);
  }
  .date-cell,
  .metric-cell {
    min-width: 0;
    padding: 9px 10px;
  }
  .date-cell {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  time {
    flex: 1;
    min-width: 0;
  }
  time :is(strong, small) {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  time strong {
    font-size: var(--type-xs);
  }
  time small {
    margin-top: 2px;
    color: var(--text-muted);
    font: var(--type-2xs) var(--font-mono);
  }
  .metric-cell {
    display: grid;
    align-content: center;
    text-align: end;
  }
  .metric-cell small {
    display: none;
  }
  .metric-cell strong {
    overflow-wrap: anywhere;
    font: var(--w-strong) var(--type-xs) / 1.35 var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .load-more {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    padding: var(--space-4);
    color: var(--text-muted);
    font: var(--type-2xs) var(--font-mono);
  }
  @container history-list (max-width: 560px) {
    .column-head {
      display: none;
    }
    .history-table {
      grid-template-columns: repeat(var(--metric-columns), minmax(0, 1fr));
      gap: 6px;
      padding: var(--space-2);
    }
    ol {
      gap: 6px;
    }
    li {
      border: 1px solid var(--border);
      border-radius: var(--r-chrome);
      background: var(--sheen), var(--surface-1);
      box-shadow: var(--elev-tile);
    }
    .date-cell {
      grid-column: 1 / -1;
      padding-block: 6px 5px;
      border-bottom: 1px solid var(--border-subtle);
    }
    time {
      display: flex;
      align-items: baseline;
      gap: var(--space-2);
    }
    .metric-cell {
      gap: 3px;
      padding: 6px 7px 7px;
      text-align: start;
    }
    .metric-cell + .metric-cell {
      border-left: 1px solid var(--border-subtle);
    }
    .metric-cell small {
      display: flex;
      align-items: center;
      gap: 4px;
      color: var(--text-muted);
      font: var(--w-heavy) var(--type-2xs) var(--font-mono);
      letter-spacing: var(--track-caps);
      text-transform: uppercase;
    }
    .metric-cell strong {
      font-size: var(--type-2xs);
    }
  }
  @container history (max-width: 560px) {
    .history-head,
    .notices {
      padding-inline: var(--space-3);
    }
    .history-head p {
      order: 3;
      flex-basis: 100%;
    }
  }
</style>
