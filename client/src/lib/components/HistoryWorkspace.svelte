<script lang="ts" module>
  // Dismissal lasts the page's lifetime and returns when the count changes.
  let dismissedMalformed = $state(0);
</script>

<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { IconName } from "../presentation/icons";
  import { onMount, tick, untrack } from "svelte";
  import { tooltip } from "../actions/tooltip";
  import { wallNow } from "../presentation/motion.svelte";
  import { canFocus, hasFocus, activeModal } from "../actions/focus";
  import { createUuid } from "../uuid";
  import {
    announceHistoryChanged,
    HistoryRepository,
    onHistoryChanged,
  } from "../history/repository";
  import { formatRecentCompletion } from "../history/format";
  import {
    fmtMs,
    fmtSpeed,
    rateUnit,
    rateValueAt,
    throughputUnitIndex,
  } from "../format";
  import { stageStatusLabel } from "../presentation/vocabulary";
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
    JARGON,
    MISSING,
    LATENCY_POPULATION,
    OUTCOME,
    STAGE,
    STATUS_TONE,
  } from "../presentation/vocabulary";
  import { announce } from "../presentation/announcer.svelte";
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
  const malformedShown = $derived(
    malformedCount > 0 && malformedCount !== dismissedMalformed,
  );
  let selectedState = $state<"ready" | "missing" | "malformed">("missing");
  let sort = $state<HistorySort>("date");
  let descending = $state(true);
  let pages = $state(1);
  let renderedAt = $state(wallNow());
  let workspace = $state<HTMLElement>();
  let list = $state<HTMLElement>();
  let detailRegion = $state<HTMLElement>();
  let previousSelectedId: string | null = null;
  let confirm = $state<
    { kind: "delete"; id: string } | { kind: "clear" } | null
  >(null);
  let confirmInvoker = $state<HTMLElement | null>(null);
  let actionError = $state("");
  let loadGeneration = 0;

  const columns = $derived(store.historyColumns);
  const prepared = $derived(prepareHistorySort(records));
  const ordered = $derived(sortPreparedHistory(prepared, sort, descending));
  const selectedIndex = $derived(
    selectedId ? ordered.findIndex((record) => record.id === selectedId) : -1,
  );
  const visibleCount = $derived(
    Math.max(pages, Math.ceil((selectedIndex + 1) / 50)) * 50,
  );
  const visible = $derived(ordered.slice(0, visibleCount));
  const selectedRecord = $derived(
    records.find((record) => record.id === selectedId) ?? null,
  );
  const span = $derived.by(() => {
    if (!records.length) return "";
    const times = records.map((record) => record.completedAt);
    const [first, last] = [Math.min(...times), Math.max(...times)];
    if (dateLabel(first) === dateLabel(last)) return dateLabel(first);
    const sameYear =
      new Date(first).getFullYear() === new Date(last).getFullYear();
    return `${sameYear ? new Date(first).toLocaleDateString(undefined, { month: "short", day: "numeric" }) : dateLabel(first)} – ${dateLabel(last)}`;
  });

  const COLUMN: Record<
    HistoryColumn,
    { short: string; icon: IconName; help: string }
  > = {
    download: { ...STAGE.download, help: JARGON.download },
    upload: { ...STAGE.upload, help: JARGON.upload },
    bidirectional: { ...STAGE.bidirectional, help: JARGON.bidirectional },
    idle: {
      short: LATENCY_POPULATION.latency.short,
      icon: STAGE.latency.icon,
      help: JARGON.latency,
    },
    loaded: {
      short: "Loaded",
      icon: STAGE.latency.icon,
      help: JARGON.loadedLatency,
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
      renderedAt = wallNow();
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
    list?.scrollTo({ top: 0 });
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
        announce("History cleared.");
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
        announce("Result deleted.");
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
  const latencyColumn = (column: HistoryColumn) =>
    column === "idle" || column === "loaded";
  // One unit per column, in its head: rates take the prefix of their median; bars share a zero-based scale.
  const scales = $derived(
    Object.fromEntries(
      columns.map((column) => {
        const values = prepared
          .flatMap((entry) => entry.keys[column] ?? [])
          .sort((a, b) => a - b);
        const tier =
          values.length && !latencyColumn(column)
            ? throughputUnitIndex(
                values[values.length >> 1],
                units.base,
                units.kind,
              )
            : undefined;
        return [
          column,
          {
            tier,
            peak: values.at(-1) ?? 0,
            unit: latencyColumn(column)
              ? "ms"
              : tier === undefined
                ? ""
                : rateUnit(units.base, units.kind, tier),
          },
        ];
      }),
    ) as Record<HistoryColumn, { tier?: number; peak: number; unit: string }>,
  );

  function metric(record: HistoryRecord, column: HistoryColumn) {
    const { stages, bidirectional } = record.result;
    const value = historyMetrics(record)[column];
    const { tier, peak } = scales[column];
    if (value != null)
      return {
        text:
          tier === undefined
            ? fmtMs(value)
            : fmtSpeed(rateValueAt(value, units.base, units.kind, tier)),
        share: peak > 0 ? value / peak : 0,
      };
    const missing = (text: string) => ({ text, share: null });
    if (column === "loaded") return missing(MISSING);
    if (column === "idle") return missing(stageStatusLabel(stages.latency));
    if (column !== "bidirectional")
      return missing(stageStatusLabel(stages[column]));
    const { survivingDirection } = bidirectionalResultPresentation(
      bidirectional?.down?.reportedBytesPerSec,
      bidirectional?.up?.reportedBytesPerSec,
    );
    return missing(
      survivingDirection
        ? `${survivingDirection === "down" ? "Down" : "Up"} only`
        : stageStatusLabel(stages.bidirectional),
    );
  }

  function historyRow(record: HistoryRecord) {
    const recent = formatRecentCompletion(record.completedAt, renderedAt);
    const exact = new Date(record.completedAt).toLocaleString(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    });
    const { outcome } = record.result;
    const metrics = columns.map((column) => metric(record, column));
    const time = new Date(record.completedAt).toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
    });
    const recentDay = ["Today", "Yesterday"].includes(
      groupHeading(record.completedAt),
    );
    const day = new Date(record.completedAt).toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
    });
    return {
      record,
      exact,
      primary: !byDay
        ? `${dateLabel(record.completedAt)}, ${time}`
        : recentDay
          ? time
          : `${day}, ${time}`,
      secondary: recent,
      outcome,
      metrics,
      label: [
        `${exact}, ${OUTCOME[outcome].toLowerCase()} result`,
        ...columns.map(
          (column, index) =>
            `${HISTORY_SORT_LABEL[column]} ${metrics[index].text}${metrics[index].share === null ? "" : ` ${scales[column].unit}`}`,
        ),
      ].join(". "),
    };
  }

  function select(event: MouseEvent, id: string) {
    if (
      event.button !== 0 ||
      event.metaKey ||
      event.ctrlKey ||
      event.shiftKey ||
      event.altKey
    )
      return;
    event.preventDefault();
    onNavigate(selectedId === id ? null : id);
  }

  const byDay = $derived(sort === "date");
  const groups = $derived.by(() => {
    const runs: { heading: string; records: HistoryRecord[] }[] = [];
    for (const record of visible) {
      const heading = byDay ? groupHeading(record.completedAt) : "";
      if (runs.at(-1)?.heading === heading) runs.at(-1)!.records.push(record);
      else runs.push({ heading, records: [record] });
    }
    return runs;
  });
  // Recent days, then months, so sparse history never gets a heading per result.
  function groupHeading(value: number): string {
    const day = (time: number) => new Date(time).toDateString();
    if (day(value) === day(renderedAt)) return "Today";
    if (day(value) === day(renderedAt - 86_400_000)) return "Yesterday";
    return new Date(value).toLocaleDateString(undefined, {
      month: "long",
      year: "numeric",
    });
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
    // Not motion: relative completion times read in minutes.
    const relativeRefresh = window.setInterval(() => {
      if (document.visibilityState === "visible") renderedAt = wallNow();
    }, 60_000);
    const stopChanges = onHistoryChanged(() => void load(false), changeSource);
    return () => {
      loadGeneration++;
      stopChanges();
      window.clearInterval(relativeRefresh);
      repository.close();
    };
  });
</script>

<section
  class="history-workspace surface enter"
  bind:this={workspace}
  aria-labelledby="history-title"
  tabindex="-1"
>
  <header class="surface-head history-head">
    <h1 id="history-title">History</h1>
    {#if records.length}
      <dl class="head-facts">
        <div>
          <dt>Results</dt>
          <dd>{records.length}</dd>
        </div>
        <div>
          <dt>Span</dt>
          <dd>{span}</dd>
        </div>
      </dl>
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
        class="btn btn-icon btn-inset close-history"
        type="button"
        aria-label="Close History"
        onclick={onClose}
      >
        <Icon name="close" />
      </button>
    </div>
  </header>

  {#if store.historyWarning || actionError || malformedShown || (records.length && !store.savingResults)}
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
      {#if malformedShown}
        <p class="notice" data-tone="warn" role="status">
          <span
            >{malformedCount} unsupported or malformed {malformedCount === 1
              ? "record was"
              : "records were"} ignored.</span
          >
          <button
            class="btn"
            type="button"
            onclick={() => {
              dismissedMalformed = malformedCount;
              workspace?.focus({ preventScroll: true });
            }}>Dismiss</button
          >
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
      <div class="history-list" bind:this={list}>
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
                <span
                  {@attach column === "date"
                    ? null
                    : tooltip(() => COLUMN[column].help)}
                  >{column === "date" ? "Date" : COLUMN[column].short}</span
                >
                {#if column !== "date" && scales[column].unit}<span class="unit"
                    >{scales[column].unit}</span
                  >{/if}
                {#if sort === column}<span class="sr-only"
                    >, {descending ? "descending" : "ascending"}</span
                  >{/if}
                <i aria-hidden="true"></i>
              </button>
              {#if column === "date"}<span class="outcome-head"></span>{/if}
            {/each}
          </div>
          {#each groups as group, index (group.heading || index)}
            <section class="day">
              {#if group.heading}<h3 id={`history-day-${index}`}>
                  {group.heading}
                </h3>{/if}
              <ol
                aria-label={group.heading ? undefined : "Saved results"}
                aria-labelledby={group.heading
                  ? `history-day-${index}`
                  : undefined}
              >
                {#each group.records as record (record.id)}
                  <li>
                    <svelte:boundary>
                      {@const row = historyRow(record)}
                      <a
                        class="result-row tile"
                        data-history-id={record.id}
                        href={`#/history/${record.id}`}
                        aria-current={selectedId === record.id
                          ? "true"
                          : undefined}
                        aria-label={row.label}
                        onclick={(event) => select(event, record.id)}
                      >
                        <time
                          datetime={new Date(record.completedAt).toISOString()}
                          title={row.exact}
                        >
                          {row.primary}
                          {#if row.secondary}<small>{row.secondary}</small>{/if}
                        </time>
                        <span class="outcome">
                          {#if row.outcome !== "complete"}<span
                              class="status-dot"
                              data-tone={STATUS_TONE[row.outcome]}
                            ></span>{OUTCOME[row.outcome]}{/if}
                        </span>
                        {#each columns as column, index (column)}
                          {@const cell = row.metrics[index]}
                          <span
                            class="metric"
                            class:missing={cell.share === null}
                            data-tone={column}
                          >
                            {#if cell.share !== null}<span
                                class="bar"
                                style:--share={cell.share}
                              ></span>{/if}
                            <span class="value">{cell.text}</span>
                          </span>
                        {/each}
                      </a>
                      {#snippet failed()}
                        <a
                          class="result-row tile"
                          data-history-id={record.id}
                          href={`#/history/${record.id}`}
                          onclick={(event) => select(event, record.id)}
                        >
                          <time
                            datetime={new Date(
                              record.completedAt,
                            ).toISOString()}
                            >{dateLabel(record.completedAt)}</time
                          >
                          <span class="outcome"
                            ><span class="status-dot" data-tone="warn"
                            ></span>Unreadable</span
                          >
                        </a>
                      {/snippet}
                    </svelte:boundary>
                  </li>
                {/each}
              </ol>
            </section>
          {/each}
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

      {#snippet unavailable(malformed: boolean)}
        <div class="detail-pane empty-state" role="status">
          <span class="empty-icon">!</span>
          <h2>
            {malformed
              ? "Unreadable saved result"
              : "Result no longer available"}
          </h2>
          <p>
            {malformed
              ? "This record uses an unsupported format or failed validation."
              : "It may have been deleted in another tab."}
          </p>
          <button class="btn" type="button" onclick={() => onNavigate(null)}
            >Back to results</button
          >
        </div>
      {/snippet}
      {#if selectedRecord}
        {#key selectedRecord.id}
          <svelte:boundary>
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
            {#snippet failed()}{@render unavailable(true)}{/snippet}
          </svelte:boundary>
        {/key}
      {:else if selectedId && selectedState !== "ready"}
        {@render unavailable(selectedState === "malformed")}
      {/if}
    </div>
  {/if}
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
    container: history / inline-size;
  }
  /* One head recipe for History and its detail: title, labelled facts, then actions. */
  .history-workspace :global(:is(.history-head, .detail-head)) {
    display: flex;
    flex: none;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-1) var(--space-4);
    min-height: 56px;
    padding: var(--space-2) var(--space-4);
  }
  .history-workspace :global(.head-facts) {
    display: flex;
    flex-wrap: wrap;
    gap: 2px var(--space-4);
    min-width: 0;
    font-size: var(--type-sm);
    line-height: 1.4;
  }
  .history-workspace :global(.head-facts > div) {
    display: flex;
    gap: 6px;
  }
  .history-workspace :global(.head-facts dt) {
    color: var(--text-soft);
  }
  .history-workspace :global(.head-facts dd) {
    font-weight: var(--w-normal);
  }
  .history-workspace :global(.outcome) {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--text-muted);
    font-size: var(--type-sm);
    white-space: nowrap;
  }
  h1 {
    font: var(--w-strong) var(--type-lg) / 1.2 var(--font-display);
    letter-spacing: var(--track-tight);
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
    padding: var(--space-3) var(--space-4) 0;
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
      grid-template-columns: minmax(360px, 2fr) minmax(460px, 3fr);
    }
    .has-detail .history-list {
      visibility: visible;
    }
    .detail-pane {
      grid-area: 1 / 2;
      border-left: var(--hairline) solid var(--border);
    }
  }
  .history-list {
    container: history-list / inline-size;
  }
  .history-table {
    display: grid;
    grid-template-columns:
      minmax(8.5rem, 1.2fr) auto
      repeat(var(--metric-columns), minmax(6rem, 1fr));
    row-gap: var(--space-5);
    padding: 0 var(--space-4) var(--space-4);
  }
  .column-head,
  .day,
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
    margin: 0 calc(-1 * var(--space-4)) calc(-1 * var(--space-3));
    padding: var(--space-2) var(--space-4) 0;
    background: var(--surface-1);
  }
  .column-head button {
    position: relative;
    display: flex;
    align-items: baseline;
    justify-content: flex-end;
    gap: 5px;
    min-width: 0;
    min-height: var(--control-h);
    padding: 7px var(--space-3);
    border-radius: var(--r-well);
    color: var(--text-muted);
    font: var(--w-strong) var(--type-sm) / 1.3 var(--font-sans);
    white-space: nowrap;
    transition: var(--transition-control);
  }
  .column-head > button:first-child {
    justify-content: flex-start;
  }
  @media (hover: hover) {
    .column-head button:hover {
      background: var(--hover-wash);
      color: var(--text);
    }
  }
  .column-head [aria-pressed="true"] {
    color: var(--text);
  }
  /* The sort mark sits in the padding, so labels share their values' edge. */
  .column-head i {
    position: absolute;
    right: 3px;
    top: calc(50% - 4px);
    width: 5px;
    height: 5px;
    border: solid var(--brand-strong);
    border-width: 0 1.5px 1.5px 0;
    opacity: 0;
    rotate: 45deg;
    transition:
      opacity var(--dur-hover) var(--ease-out),
      rotate var(--dur-hover) var(--ease-out);
  }
  .column-head > button:first-child i {
    position: static;
    align-self: center;
    margin-left: 2px;
  }
  .column-head [data-order] i {
    opacity: 1;
  }
  .column-head [data-order="ascending"] i {
    rotate: 225deg;
  }
  .head-icon {
    display: grid;
    flex: none;
    align-self: center;
    color: var(--tone);
  }
  .head-icon :global(svg) {
    width: var(--icon-sm);
    height: var(--icon-sm);
  }
  .unit {
    color: var(--text-soft);
    font: var(--w-strong) var(--type-xs) var(--font-mono);
  }
  .day {
    row-gap: 6px;
  }
  .day > h3 {
    grid-column: 1 / -1;
    padding-inline: var(--space-3);
    color: var(--text-muted);
    font: var(--w-strong) var(--type-sm) / 1.3 var(--font-sans);
  }
  ol {
    overflow: hidden;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
  }
  li + li {
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .result-row {
    align-items: center;
    min-height: var(--control-h);
    font-size: var(--type-body);
    line-height: 1.4;
  }
  .result-row:focus-visible {
    outline-offset: -2px;
  }
  .result-row > * {
    min-width: 0;
    padding: 6px var(--space-3);
  }
  time {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0 var(--space-2);
    font-weight: var(--w-normal);
  }
  time small {
    color: var(--text-soft);
    font-size: var(--type-xs);
  }
  .outcome:empty {
    padding: 0;
  }
  .metric {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-2);
  }
  /* A magnitude, never a verdict: the column's largest value fills the bar. */
  .bar {
    flex: 0 1 4.5rem;
    height: 3px;
    border-radius: var(--r-full);
    background: linear-gradient(
        270deg,
        color-mix(in oklab, var(--tone) 60%, transparent)
          calc(var(--share) * 100%),
        transparent 0
      )
      no-repeat;
  }
  .value {
    font-weight: var(--w-strong);
    white-space: nowrap;
  }
  .missing .value {
    color: var(--text-soft);
    font-weight: var(--w-normal);
  }
  .load-more {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    padding: 0 var(--space-4) var(--space-5);
    color: var(--text-soft);
    font-size: var(--type-sm);
  }
  @container history-list (max-width: 900px) {
    .bar {
      display: none;
    }
  }
  @container history-list (max-width: 560px) {
    .history-table {
      grid-template-columns: repeat(var(--metric-columns), minmax(0, 1fr));
      row-gap: var(--space-4);
      padding-inline: var(--space-3);
    }
    .column-head {
      margin: 0 calc(-1 * var(--space-3)) calc(-1 * var(--space-2));
      padding-inline: var(--space-3);
    }
    .column-head > button:first-child,
    .outcome-head {
      display: none;
    }
    .head-icon {
      display: none;
    }
    .column-head button {
      flex-wrap: wrap;
      align-content: center;
      gap: 0 4px;
      padding: 6px var(--space-2);
      font-size: var(--type-xs);
    }
    .column-head button .unit {
      flex-basis: 100%;
      text-align: end;
    }
    .result-row {
      position: relative;
      padding-block: 2px 4px;
    }
    .result-row > * {
      padding: 4px var(--space-2);
    }
    time {
      grid-column: 1 / -1;
      padding-inline: var(--space-3) 7rem;
    }
    .outcome {
      position: absolute;
      top: 4px;
      right: var(--space-1);
    }
  }
  @container history (max-width: 560px) {
    .history-workspace :global(.head-facts) {
      order: 3;
      flex-basis: 100%;
    }
    .history-workspace:has(.has-detail) .history-head .head-facts {
      display: none;
    }
  }
  @container history (max-width: 560px) {
    .history-workspace :global(:is(.history-head, .detail-head)),
    .notices {
      padding-inline: var(--space-3);
    }
  }
</style>
