<script lang="ts">
  import { observeWidth } from "../actions/observeWidth";
  import { onMount, tick } from "svelte";
  import { bidirectionalResultPresentation } from "../presentation/bidirectionalResult";
  import { canFocus, hasFocus, activeModal } from "../actions/focus";
  import { ICON } from "../constants";
  import { HistoryRepository } from "../history/repository";
  import { broadcastHistory, historyChanges } from "../history/changes";
  import {
    formatHistoryRate,
    formatLatency,
    formatRecentCompletion,
    stageStatusLabel,
  } from "../history/format";
  import {
    naturalDescending,
    prepareHistorySort,
    sortPreparedHistory,
    type HistorySort,
  } from "../history/sort";
  import {
    HISTORY_LIMIT,
    type HistoryRecord,
    type StageStatus,
  } from "../history/types";
  import type { HistoryColumn } from "../state/persistence";
  import { store } from "../state/store.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import HistoryManagementControl from "./history/HistoryManagementControl.svelte";
  import HistoryResultDetail, {
    type ServerView,
  } from "./history/HistoryResultDetail.svelte";
  import HistoryViewControl from "./history/HistoryViewControl.svelte";

  interface Props {
    selectedId: string | null;
    onNavigate: (id: string | null) => void;
    onClose: () => void;
  }

  let { selectedId, onNavigate, onClose }: Props = $props();
  const repository = new HistoryRepository();
  let loadState = $state<"loading" | "ready" | "error">("loading");
  let records = $state.raw<HistoryRecord[]>([]);
  let malformedCount = $state(0);
  let selectedState = $state<"ready" | "missing" | "malformed">("missing");
  let sort = $state<HistorySort>("date");
  let descending = $state(true);
  let visibleCount = $state(50);
  let renderedAt = $state(Date.now());
  let workspaceWidth = $state(0);
  let workspace = $state<HTMLElement>();
  let serverView = $state<ServerView | null>(null);
  let detailRegion = $state<HTMLElement>();
  let detailCloseButton = $state<HTMLButtonElement>();
  let requestedDetailFocus = $state<{
    id: string;
    target: "region" | "close" | "none";
  } | null>(null);
  let focusedDetailId: string | null = null;
  let previousSelectedId: string | null = null;
  let keyboardActivationId: string | null = null;
  let confirm = $state<
    { kind: "delete"; id: string } | { kind: "clear" } | null
  >(null);
  let confirmInvoker = $state<HTMLElement | null>(null);
  let actionError = $state("");
  let announcement = $state("");
  let loadGeneration = 0;

  const columns = $derived(store.historyColumns);
  const preparedRecords = $derived(prepareHistorySort(records));
  const ordered = $derived(
    sortPreparedHistory(preparedRecords, sort, descending),
  );
  const visibleRows = $derived(
    ordered.slice(0, visibleCount).map((record) => historyRow(record)),
  );
  const selectedRecord = $derived(
    selectedId
      ? (records.find((record) => record.id === selectedId) ?? null)
      : null,
  );
  const sideInspector = $derived(workspaceWidth >= 1040);
  const oldest = $derived(
    records.length
      ? Math.min(...records.map((record) => record.completedAt))
      : null,
  );
  const newest = $derived(
    records.length
      ? Math.max(...records.map((record) => record.completedAt))
      : null,
  );

  const columnMeta: Record<
    HistoryColumn,
    { short: string; icon: string; sort: HistorySort }
  > = {
    download: {
      short: "Down",
      icon: ICON.download,
      sort: "download",
    },
    upload: {
      short: "Up",
      icon: ICON.upload,
      sort: "upload",
    },
    bidirectional: {
      short: "Bi-dir",
      icon: ICON.bidirectional,
      sort: "bidirectional",
    },
    idle: {
      short: "Idle",
      icon: ICON.ping,
      sort: "idle",
    },
    loaded: {
      short: "Loaded",
      icon: ICON.ping,
      sort: "loaded",
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

  function closeDetail() {
    onNavigate(null);
  }

  function activate(record: HistoryRecord, keyboard: boolean) {
    if (selectedId === record.id) {
      closeDetail();
      return;
    }
    requestedDetailFocus = {
      id: record.id,
      target: keyboard ? "close" : "none",
    };
    onNavigate(record.id);
  }

  function setSort(next: HistorySort, nextDescending: boolean) {
    sort = next;
    descending = nextDescending;
    visibleCount = 50;
  }

  function sortColumn(next: HistorySort) {
    setSort(next, sort === next ? !descending : naturalDescending(next));
  }

  function loadMore() {
    visibleCount = Math.min(ordered.length, visibleCount + 50);
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
        const generation = await repository.clear();
        records = [];
        malformedCount = 0;
        announcement = "History cleared.";
        if (owner?.isConnected && selectedId) onNavigate(null);
        const change = { type: "clear" as const, generation };
        broadcastHistory(change);
        window.dispatchEvent(
          new CustomEvent("graphite-meter-history-changed", {
            detail: change,
          }),
        );
      } else {
        await repository.delete(action.id);
        records = records.filter((record) => record.id !== action.id);
        announcement = "Result deleted.";
        if (owner?.isConnected && selectedId === action.id) onNavigate(null);
        broadcastHistory({ type: "delete", id: action.id });
      }
      if (action.kind !== "clear")
        window.dispatchEvent(new Event("graphite-meter-history-changed"));
      if (action.kind === "clear") {
        confirmInvoker = null;
        await tick();
        const target = workspace?.querySelector<HTMLElement>(".close-history");
        if (!hasFocus() && canFocus(target))
          target.focus({ preventScroll: true });
      }
    } catch {
      actionError = "The local archive could not be changed. Try again.";
      confirmInvoker = null;
    }
  }

  function requestClear(invoker: HTMLElement) {
    confirmInvoker = invoker;
    confirm = { kind: "clear" };
  }

  function cancelConfirmation() {
    confirm = null;
    confirmInvoker = null;
  }

  function partial(record: HistoryRecord): boolean {
    return (
      record.failures.length > 0 ||
      [
        record.stages.latency.status,
        record.stages.download.status,
        record.stages.upload.status,
        record.stages.bidirectional.status,
      ].some((status) => status === "partial" || status === "failed")
    );
  }

  function rate(value: number | null | undefined): string {
    return formatHistoryRate(value, {
      base: store.unitBase,
      kind: store.unitKind,
    });
  }

  function resultRate(
    status: StageStatus,
    value: number | null | undefined,
  ): string {
    return value == null ? stageStatusLabel(status) : rate(value);
  }

  function bidiRate(record: HistoryRecord): string {
    const result = record.stages.bidirectional;
    const model = bidirectionalResultPresentation(
      result.down?.reportedBytesPerSec,
      result.up?.reportedBytesPerSec,
    );
    if (model.combinedBytesPerSec != null)
      return rate(model.combinedBytesPerSec);
    if (model.survivingDirection)
      return model.survivingDirection === "down" ? "Down only" : "Up only";
    return stageStatusLabel(result.status);
  }

  function loadedMetric(record: HistoryRecord): string {
    if (record.bufferbloat) return formatLatency(record.bufferbloat.loadedMs);
    const transferStatuses = [
      record.stages.download.status,
      record.stages.upload.status,
      record.stages.bidirectional.status,
    ];
    if (
      record.stages.latency.status === "not-run" ||
      transferStatuses.every((status) => status === "not-run")
    )
      return "Not run";
    return "Unavailable";
  }

  interface HistoryRowView {
    record: HistoryRecord;
    exactDate: string;
    primaryDate: string;
    secondaryDate: string;
    partial: boolean;
    metrics: Record<HistoryColumn, string>;
    ariaLabel: string;
  }

  function historyRow(record: HistoryRecord): HistoryRowView {
    const exactDate = fullDate(record.completedAt);
    const recentDate = formatRecentCompletion(record.completedAt, renderedAt);
    const metrics: Record<HistoryColumn, string> = {
      download: resultRate(
        record.stages.download.status,
        record.stages.download.result?.reportedBytesPerSec,
      ),
      upload: resultRate(
        record.stages.upload.status,
        record.stages.upload.result?.reportedBytesPerSec,
      ),
      bidirectional: bidiRate(record),
      idle: record.stages.latency.result
        ? formatLatency(record.stages.latency.result.reportedMs)
        : stageStatusLabel(record.stages.latency.status),
      loaded: loadedMetric(record),
    };
    const isPartial = partial(record);
    return {
      record,
      exactDate,
      primaryDate: recentDate ?? dateLabel(record.completedAt),
      secondaryDate: recentDate
        ? dateLabel(record.completedAt)
        : new Date(record.completedAt).toLocaleTimeString(undefined, {
            hour: "2-digit",
            minute: "2-digit",
          }),
      partial: isPartial,
      metrics,
      ariaLabel: `${exactDate}${isPartial ? ", partial result" : ", complete result"}. Download ${metrics.download}. Upload ${metrics.upload}. Bidirectional ${metrics.bidirectional}. Idle ${metrics.idle}. Loaded ${metrics.loaded}.`,
    };
  }

  function dateLabel(value: number): string {
    return new Date(value).toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
      year: "numeric",
    });
  }

  function fullDate(value: number): string {
    return new Date(value).toLocaleString(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    });
  }

  $effect(() => {
    const id = selectedId;
    if (loadState !== "ready") return;
    void resolveSelection(id, loadGeneration);
    if (!id) {
      focusedDetailId = null;
      requestedDetailFocus = null;
    }
  });

  $effect(() => {
    const id = selectedId;
    const previous = previousSelectedId;
    previousSelectedId = id;
    if (!previous || id) return;
    const target =
      workspace?.querySelector<HTMLElement>(
        `[data-history-id="${previous}"]`,
      ) ?? workspace?.querySelector<HTMLElement>(".close-history");
    if (!activeModal() && canFocus(target)) target.focus();
  });

  $effect(() => {
    const id = selectedRecord?.id;
    const region = detailRegion;
    const closeButton = detailCloseButton;
    if (!id || focusedDetailId === id) return;
    const request =
      requestedDetailFocus?.id === id ? requestedDetailFocus.target : "region";
    if (request === "none") {
      focusedDetailId = id;
      requestedDetailFocus = null;
      return;
    }
    const target = request === "close" ? closeButton : region;
    if (!target) return;
    focusedDetailId = id;
    requestedDetailFocus = null;
    if (!activeModal() && canFocus(target))
      target.focus({ preventScroll: request === "region" && sideInspector });
  });

  $effect(() => {
    const id = selectedId;
    void sort;
    void descending;
    void records;
    const index = id ? ordered.findIndex((record) => record.id === id) : -1;
    if (index >= visibleCount) visibleCount = Math.ceil((index + 1) / 50) * 50;
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
    const stopChanges = historyChanges(refresh);
    window.addEventListener("graphite-meter-history-changed", refresh);
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refreshWhenVisible);
    return () => {
      loadGeneration++;
      stopChanges();
      window.removeEventListener("graphite-meter-history-changed", refresh);
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
  {@attach observeWidth((width) => (workspaceWidth = width))}
  aria-labelledby="history-title"
  tabindex="-1"
>
  <header class="surface-head history-head">
    <div class="history-title">
      <div>
        <h1 id="history-title">History</h1>
        <p>Saved on this device</p>
      </div>
    </div>
    <button
      class="btn close-history"
      type="button"
      aria-label="Close History"
      onclick={onClose}
    >
      <span>{@html ICON.close}</span><strong>Close</strong>
    </button>
  </header>

  {#if store.historyWarning || actionError || malformedCount}
    <div class="notice archive-warning" data-tone="warn" role="status">
      <span aria-hidden="true">!</span>
      <div>
        {#if store.historyWarning}<p>{store.historyWarning}</p>{/if}
        {#if actionError}<p>{actionError}</p>{/if}
        {#if malformedCount}<p>
            {malformedCount} unsupported or malformed {malformedCount === 1
              ? "record was"
              : "records were"} ignored.
          </p>{/if}
      </div>
    </div>
  {/if}

  {#if records.length > 0 && !store.savingResults}
    <div class="notice saving-notice" data-tone="warn">
      <p>
        <strong>Saving is paused.</strong> Retained results remain available.
      </p>
      <button
        class="btn"
        type="button"
        onclick={() => (store.resultHistoryPreference = "enabled")}
      >
        Enable future saves
      </button>
    </div>
  {/if}

  {#if loadState === "loading"}
    <div class="empty-state" role="status">
      <span class="empty-icon">{@html ICON.history}</span>
      <h2>Opening local archive</h2>
      <p>Reading saved results from this browser.</p>
    </div>
  {:else if loadState === "error"}
    <div class="empty-state" data-tone="err" role="alert">
      <span class="empty-icon">!</span>
      <h2>History is unavailable</h2>
      <p>The browser could not open its local result store.</p>
      <button class="btn btn-accent" type="button" onclick={() => load()}
        >Retry</button
      >
    </div>
  {:else if records.length === 0}
    <div class="empty-state">
      <span class="empty-icon">{@html ICON.history}</span>
      <h2>No saved results</h2>
      {#if store.savingResults}
        <p>Completed tests will appear here automatically.</p>
      {:else}
        <p>
          Saving is paused. Enable it to keep future completed tests on this
          device.
        </p>
        <button
          class="btn btn-accent"
          type="button"
          onclick={() => (store.resultHistoryPreference = "enabled")}
        >
          Enable result history
        </button>
      {/if}
      {#if malformedCount > 0}
        <div class="empty-management">
          <HistoryManagementControl onClear={requestClear} />
        </div>
      {/if}
    </div>
  {:else}
    <div class="surface-head archive-overview" aria-label="History overview">
      <div class="overview-primary">
        <strong>{records.length}</strong>
        <span class="caps"
          >{records.length === 1 ? "result" : "results"} saved locally</span
        >
      </div>
      <div class="overview-dates">
        <span class="caps">Archive span</span>
        <strong
          >{oldest == null ? "—" : dateLabel(oldest)} <i>to</i>
          {newest == null ? "—" : dateLabel(newest)}</strong
        >
      </div>
    </div>

    <div class="archive-toolbar">
      <p>
        <strong>Results</strong>
      </p>
      <div class="toolbar-actions">
        <HistoryViewControl
          {columns}
          {sort}
          {descending}
          compact={workspaceWidth <= 820}
          onColumnsChange={(next) => (store.historyColumns = next)}
          onSortChange={setSort}
        />
        <HistoryManagementControl onClear={requestClear} />
      </div>
    </div>

    <div
      class="workspace-body"
      class:wide-layout={sideInspector}
      class:with-side={sideInspector && selectedId !== null}
    >
      <div class="archive-list" aria-label="Saved results">
        <div
          class="column-head"
          role="row"
          style={`--metric-columns:${columns.length}`}
        >
          <span
            role="columnheader"
            aria-sort={sort === "date"
              ? descending
                ? "descending"
                : "ascending"
              : "none"}
          >
            <button type="button" onclick={() => sortColumn("date")}>
              <span>Date</span><i aria-hidden="true"></i>
            </button>
          </span>
          {#each columns as column}
            <span
              role="columnheader"
              data-tone={column}
              aria-sort={sort === columnMeta[column].sort
                ? descending
                  ? "descending"
                  : "ascending"
                : "none"}
            >
              <button
                type="button"
                onclick={() => sortColumn(columnMeta[column].sort)}
              >
                <span class="head-icon">{@html columnMeta[column].icon}</span>
                <span>{columnMeta[column].short}</span>
                <i aria-hidden="true"></i>
              </button>
            </span>
          {/each}
        </div>
        <ol>
          {#each visibleRows as row (row.record.id)}
            {@const record = row.record}
            <li class:selected={selectedId === record.id}>
              <a
                class="result-row"
                data-history-id={record.id}
                style={`--metric-columns:${columns.length}`}
                href={`#/history/${record.id}`}
                aria-current={selectedId === record.id ? "true" : undefined}
                aria-expanded={selectedId === record.id}
                aria-label={row.ariaLabel}
                onkeydown={(event) => {
                  if (
                    event.key === "Enter" &&
                    !event.metaKey &&
                    !event.ctrlKey &&
                    !event.shiftKey &&
                    !event.altKey
                  )
                    keyboardActivationId = record.id;
                }}
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
                  const keyboard = keyboardActivationId === record.id;
                  keyboardActivationId = null;
                  activate(record, keyboard);
                }}
              >
                <span class="date-cell">
                  <time datetime={new Date(record.completedAt).toISOString()}>
                    <strong title={row.exactDate}>{row.primaryDate}</strong>
                    <small>{row.secondaryDate}</small>
                  </time>
                  <span class="row-badges">
                    {#if row.partial}<em class="badge" data-tone="warn"
                        >Partial</em
                      >{/if}
                    {#if selectedId === record.id}<em
                        class="badge"
                        data-tone="brand">Selected</em
                      >{/if}
                  </span>
                </span>
                <span class="metrics-row">
                  {#each columns as column}
                    <span class="metric-cell" data-tone={column}>
                      <small
                        ><span>{@html columnMeta[column].icon}</span
                        >{columnMeta[column].short}</small
                      >
                      <strong title={row.metrics[column]}
                        >{row.metrics[column]}</strong
                      >
                    </span>
                  {/each}
                </span>
              </a>
              {#if selectedId === record.id && selectedRecord && !sideInspector}
                <div class="inline-inspector enter">
                  <HistoryResultDetail
                    record={selectedRecord}
                    onClose={closeDetail}
                    onDelete={() =>
                      (confirm = { kind: "delete", id: record.id })}
                    bind:serverView
                    bind:region={detailRegion}
                    bind:closeButton={detailCloseButton}
                  />
                </div>
              {/if}
            </li>
          {/each}
        </ol>
        {#if visibleCount < ordered.length}
          <div class="load-more" {@attach loadMoreWhenVisible}>
            <button class="btn" type="button" onclick={loadMore}
              >Load 50 more</button
            >
            <span>{visibleCount} of {ordered.length}</span>
          </div>
        {/if}
      </div>

      {#if sideInspector && selectedRecord}
        <aside class="detail-inspector enter" aria-label="Selected result">
          <HistoryResultDetail
            record={selectedRecord}
            onClose={closeDetail}
            onDelete={() =>
              (confirm = { kind: "delete", id: selectedRecord.id })}
            bind:serverView
            bind:region={detailRegion}
            bind:closeButton={detailCloseButton}
          />
        </aside>
      {:else if selectedId && !selectedRecord && selectedState !== "ready"}
        <aside
          class="selection-state"
          class:side-state={sideInspector}
          role="status"
        >
          <span>{selectedState === "malformed" ? "!" : "×"}</span>
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
          <button class="btn" type="button" onclick={closeDetail}
            >Back to results</button
          >
        </aside>
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
    ? "Permanently remove all locally stored results from this browser?"
    : "Permanently remove this saved result from this browser?"}
  confirmLabel={confirm?.kind === "clear" ? "Clear history" : "Delete result"}
  onCancel={cancelConfirmation}
  onConfirm={confirmAction}
/>

<style>
  .history-workspace {
    position: relative;
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
    container-type: inline-size;
  }
  .history-workspace:focus {
    outline: none;
  }
  .history-head,
  .archive-overview,
  .archive-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    padding: 10px var(--space-4);
  }
  .history-title {
    min-width: 0;
  }
  h1 {
    font: 700 var(--type-lg) / 1 var(--font-display);
    letter-spacing: -0.015em;
  }
  .history-title p {
    margin-top: 3px;
    color: var(--text-muted);
    font-size: var(--type-2xs);
    line-height: 1;
  }
  .notice {
    align-items: center;
    padding-inline: var(--space-4);
    border-width: 0 0 1px;
    border-radius: 0;
  }
  .archive-warning > span {
    display: grid;
    flex: none;
    place-items: center;
    width: 18px;
    height: 18px;
    border: 1px solid currentColor;
    border-radius: var(--r-full);
    color: var(--warn);
    font: var(--w-heavy) var(--type-2xs) var(--font-mono);
  }
  .archive-warning > div {
    display: grid;
    gap: 2px;
    color: var(--text-muted);
  }
  .saving-notice {
    justify-content: space-between;
    color: var(--text-muted);
  }
  .empty-state {
    min-height: 360px;
  }
  .empty-management {
    margin-top: var(--space-2);
  }
  .archive-overview {
    border-bottom-color: var(--border);
  }
  .archive-overview > div {
    min-width: 0;
  }
  .archive-overview strong {
    display: block;
    overflow-wrap: anywhere;
    font: var(--w-strong) var(--type-xs) var(--font-mono);
  }
  .overview-primary {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
  }
  .archive-overview .overview-primary strong {
    color: var(--brand-strong);
    font-size: var(--type-xl);
    line-height: 1;
  }
  .overview-dates {
    text-align: end;
  }
  .overview-dates strong {
    margin-top: 3px;
  }
  .archive-overview i {
    color: var(--text-muted);
    font-style: normal;
  }
  .archive-toolbar {
    gap: var(--space-3);
    padding-block: var(--space-2);
    border-bottom: 1px solid var(--border);
    font-size: var(--type-xs);
  }
  .toolbar-actions {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .workspace-body {
    display: grid;
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior-y: contain;
  }
  .workspace-body.wide-layout {
    overflow: hidden;
  }
  .workspace-body.with-side {
    grid-template-columns: minmax(580px, 1fr) minmax(380px, 0.66fr);
  }
  .archive-list {
    position: relative;
    min-width: 0;
    background: var(--surface-1);
    isolation: isolate;
  }
  .wide-layout .archive-list {
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior-y: contain;
  }
  .with-side .archive-list {
    border-right: 1px solid var(--border-strong);
  }
  .column-head,
  .result-row {
    display: grid;
    grid-template-columns:
      minmax(150px, 1.25fr)
      repeat(var(--metric-columns), minmax(82px, 1fr));
    min-width: 0;
  }
  .metrics-row {
    display: contents;
  }
  .column-head {
    position: sticky;
    top: 0;
    z-index: 5;
    border-bottom: 1px solid var(--border-strong);
    background: var(--sheen), var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  .column-head > span {
    min-width: 0;
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
    font: 700 var(--type-2xs) var(--font-mono);
    letter-spacing: var(--track-caps);
    text-transform: uppercase;
    transition: var(--transition-control);
  }
  .column-head > span:first-child button {
    justify-content: flex-start;
  }
  @media (hover: hover) {
    .column-head button:hover {
      background: var(--brand-soft);
      color: var(--text);
    }
    .column-head button:hover i::after {
      opacity: 0.35;
    }
  }
  .column-head [aria-sort]:not([aria-sort="none"]) button {
    color: var(--brand-strong);
  }
  .column-head i {
    position: relative;
    flex: none;
    width: 9px;
    height: 12px;
    color: var(--brand-strong);
  }
  .column-head i::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 4px;
    height: 4px;
    border: solid currentColor;
    border-width: 0 1.5px 1.5px 0;
    opacity: 0;
    rotate: 45deg;
    transition: opacity var(--dur-hover) var(--ease-out);
  }
  .column-head [aria-sort="descending"] i::after,
  .column-head [aria-sort="ascending"] i::after {
    opacity: 1;
  }
  .column-head [aria-sort="ascending"] i::after {
    top: 5px;
    rotate: 225deg;
  }
  .head-icon,
  .metric-cell small span {
    display: grid;
    color: var(--tone, var(--text-soft));
  }
  .head-icon :global(svg),
  .metric-cell small :global(svg) {
    width: var(--icon-sm);
    height: var(--icon-sm);
  }
  ol {
    padding: 0 var(--space-2) var(--space-2);
  }
  li {
    min-width: 0;
    border-bottom: 1px solid var(--border-subtle);
    content-visibility: auto;
    contain-intrinsic-size: auto 58px;
  }
  li.selected {
    content-visibility: visible;
  }
  .result-row {
    position: relative;
    min-height: 56px;
    transition: var(--transition-control);
  }
  @media (hover: hover) {
    .result-row:hover {
      background: var(--surface-2);
    }
  }
  .result-row[aria-current="true"] {
    background: var(--surface-2);
    box-shadow: inset 2px 0 0 var(--brand);
  }
  .date-cell,
  .metric-cell {
    min-width: 0;
    padding: 10px;
  }
  .date-cell {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .date-cell time {
    flex: 1;
    min-width: 0;
  }
  .date-cell time strong,
  .date-cell time small {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .date-cell time strong {
    font-size: var(--type-xs);
  }
  .date-cell time small {
    margin-top: 2px;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-2xs) var(--font-mono);
  }
  .row-badges {
    display: grid;
    justify-items: end;
    gap: 3px;
  }
  .row-badges em {
    font-style: normal;
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
    font: 620 var(--type-xs) / 1.35 var(--font-mono);
  }
  .detail-inspector {
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior-y: contain;
  }
  .inline-inspector {
    border-top: 2px solid color-mix(in srgb, var(--brand) 50%, var(--border));
  }
  .selection-state {
    display: grid;
    justify-items: start;
    align-content: start;
    gap: 7px;
    padding: var(--space-5);
    border-top: 1px solid var(--border-strong);
  }
  .selection-state.side-state {
    position: sticky;
    top: 0;
    border-top: 0;
  }
  .selection-state > span {
    color: var(--warn);
    font: var(--w-heavy) var(--type-lg) var(--font-mono);
  }
  .selection-state h2 {
    font-size: var(--type-md);
  }
  .selection-state p {
    color: var(--text-muted);
    font-size: var(--type-sm);
    line-height: 1.45;
  }
  .load-more {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    padding: var(--space-4);
  }
  .load-more span {
    color: var(--text-muted);
    font: 600 var(--type-2xs) var(--font-mono);
  }
  @container (max-width: 820px) {
    .column-head {
      display: none;
    }
    ol {
      display: grid;
      gap: 6px;
      padding: var(--space-2);
    }
    li {
      border: 1px solid var(--border);
      border-radius: var(--r-chrome);
      background: var(--sheen), var(--surface-1);
      box-shadow: var(--elev-tile);
      contain-intrinsic-size: auto 76px;
    }
    li.selected {
      border-color: var(--border-strong);
    }
    .result-row {
      grid-template-columns: minmax(0, 1fr);
      min-height: 72px;
    }
    .date-cell {
      min-height: 31px;
      padding: 6px 9px 5px;
      border-bottom: 1px solid var(--border);
      background: color-mix(in srgb, var(--surface-2) 72%, transparent);
    }
    .date-cell time {
      display: flex;
      align-items: baseline;
      gap: var(--space-2);
    }
    .date-cell time strong,
    .date-cell time small {
      margin: 0;
    }
    .date-cell time small {
      flex: none;
    }
    .row-badges {
      display: flex;
      align-items: center;
      gap: var(--space-1);
    }
    .metrics-row {
      display: grid;
      grid-template-columns: repeat(var(--metric-columns), minmax(0, 1fr));
      min-width: 0;
    }
    .metric-cell {
      gap: 3px;
      padding: 6px 7px 7px;
      border-left: 1px solid var(--border-subtle);
      text-align: start;
    }
    .metric-cell:first-child {
      border-left: 0;
    }
    .metric-cell small {
      display: flex;
      align-items: center;
      gap: 5px;
      color: var(--text-muted);
      font: 700 var(--type-2xs) var(--font-mono);
      letter-spacing: var(--track-caps);
      text-transform: uppercase;
    }
    .metric-cell strong {
      overflow: hidden;
      font-size: var(--type-2xs);
      text-overflow: ellipsis;
      white-space: nowrap;
    }
  }
  @container (max-width: 560px) {
    .history-head {
      padding-inline: var(--space-3);
    }
    .close-history strong {
      display: none;
    }
    .archive-overview {
      display: grid;
      grid-template-columns: minmax(88px, 0.65fr) minmax(0, 1.35fr);
      gap: var(--space-3);
    }
    .archive-toolbar {
      align-items: flex-start;
      padding-inline: var(--space-3);
    }
    .saving-notice {
      align-items: flex-start;
    }
  }
  @container (max-width: 330px) {
    .archive-toolbar {
      display: grid;
    }
    .toolbar-actions {
      justify-content: space-between;
    }
    .metric-cell {
      padding-inline: 5px;
    }
    .metric-cell small {
      gap: 3px;
      letter-spacing: 0;
    }
  }
</style>
