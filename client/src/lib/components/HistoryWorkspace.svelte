<script lang="ts">
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
  import HistoryResultDetail from "./history/HistoryResultDetail.svelte";
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
    if (typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) loadMore();
      },
      { rootMargin: "200px 0px" },
    );
    observer.observe(node);
    return { destroy: () => observer.disconnect() };
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
  class="history-workspace"
  bind:this={workspace}
  bind:clientWidth={workspaceWidth}
  aria-labelledby="history-title"
  tabindex="-1"
>
  <header class="history-head">
    <div class="history-title">
      <div>
        <h1 id="history-title">History</h1>
        <p>Saved on this device</p>
      </div>
    </div>
    <button
      class="close-history"
      type="button"
      aria-label="Close History"
      onclick={onClose}
    >
      <span>{@html ICON.close}</span><strong>Close</strong>
    </button>
  </header>

  {#if store.historyWarning || actionError || malformedCount}
    <div class="archive-warning" role="status">
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
    <div class="saving-notice">
      <p>
        <strong>Saving is paused.</strong> Retained results remain available.
      </p>
      <button
        type="button"
        onclick={() => (store.resultHistoryPreference = "enabled")}
      >
        Enable future saves
      </button>
    </div>
  {/if}

  {#if loadState === "loading"}
    <div class="archive-state" role="status">
      <span class="state-icon">{@html ICON.history}</span>
      <h2>Opening local archive</h2>
      <p>Reading saved results from this browser.</p>
    </div>
  {:else if loadState === "error"}
    <div class="archive-state error" role="alert">
      <span class="state-icon">!</span>
      <h2>History is unavailable</h2>
      <p>The browser could not open its local result store.</p>
      <button type="button" onclick={() => load()}>Retry</button>
    </div>
  {:else if records.length === 0}
    <div class="archive-state">
      <span class="state-icon">{@html ICON.history}</span>
      <h2>No saved results</h2>
      {#if store.savingResults}
        <p>Completed tests will appear here automatically.</p>
      {:else}
        <p>
          Saving is paused. Enable it to keep future completed tests on this
          device.
        </p>
        <button
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
    <div class="archive-overview" aria-label="History overview">
      <div class="overview-primary">
        <strong>{records.length}</strong>
        <span>{records.length === 1 ? "result" : "results"} saved locally</span>
      </div>
      <div class="overview-dates">
        <span>Archive span</span>
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
                    {#if row.partial}<em class="partial">Partial</em>{/if}
                    {#if selectedId === record.id}<em class="selected-badge"
                        >Selected</em
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
                <div class="inline-inspector">
                  <HistoryResultDetail
                    record={selectedRecord}
                    onClose={closeDetail}
                    onDelete={() =>
                      (confirm = { kind: "delete", id: record.id })}
                    bind:region={detailRegion}
                    bind:closeButton={detailCloseButton}
                  />
                </div>
              {/if}
            </li>
          {/each}
        </ol>
        {#if visibleCount < ordered.length}
          <div class="load-more" use:loadMoreWhenVisible>
            <button type="button" onclick={loadMore}>Load 50 more</button>
            <span>{visibleCount} of {ordered.length}</span>
          </div>
        {/if}
      </div>

      {#if sideInspector && selectedRecord}
        <aside class="detail-inspector" aria-label="Selected result">
          <HistoryResultDetail
            record={selectedRecord}
            onClose={closeDetail}
            onDelete={() =>
              (confirm = { kind: "delete", id: selectedRecord.id })}
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
          <button type="button" onclick={closeDetail}>Back to results</button>
        </aside>
      {/if}
    </div>
  {/if}

  <p class="sr-status" aria-live="polite">{announcement}</p>
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
    container-type: inline-size;
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
  }
  .history-workspace:focus {
    outline: none;
  }
  h1,
  h2,
  p {
    margin: 0;
  }
  .history-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    padding: 10px var(--space-4);
    border-bottom: 1px solid var(--border-strong);
    background:
      linear-gradient(180deg, var(--surface-2), var(--surface-1) 72%),
      var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  .history-title {
    display: flex;
    align-items: center;
    min-width: 0;
  }
  h1 {
    font-family: var(--font-display);
    font-size: var(--type-lg);
    font-weight: 700;
    letter-spacing: -0.015em;
    line-height: 1;
  }
  .history-title p {
    margin-top: 3px;
    color: var(--text-muted);
    font-size: 9px;
    line-height: 1;
  }
  .close-history {
    display: inline-flex;
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
  .close-history:hover {
    border-color: var(--border-strong);
    color: var(--text);
  }
  .close-history span {
    display: grid;
  }
  .close-history :global(svg) {
    width: 14px;
    height: 14px;
  }
  .archive-warning,
  .saving-notice {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 8px var(--space-4);
    border-bottom: 1px solid var(--border);
    font-size: var(--type-xs);
  }
  .archive-warning {
    background: var(--warn-soft);
    color: var(--warn);
  }
  .archive-warning > span {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    flex: none;
    border: 1px solid currentColor;
    border-radius: var(--r-full);
    font: 750 10px var(--font-mono);
  }
  .archive-warning > div {
    display: grid;
    gap: 2px;
  }
  .archive-warning p {
    color: var(--text-muted);
  }
  .saving-notice {
    justify-content: space-between;
    background: color-mix(in srgb, var(--warn) 6%, var(--surface-1));
    color: var(--text-muted);
  }
  .saving-notice strong {
    color: var(--warn);
  }
  .saving-notice button,
  .archive-state button,
  .selection-state button,
  .load-more button {
    min-height: 30px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    color: var(--text);
    font-size: var(--type-xs);
    font-weight: 700;
    cursor: pointer;
  }
  .archive-state {
    display: grid;
    justify-items: center;
    align-content: center;
    min-height: 360px;
    padding: var(--space-6);
    text-align: center;
  }
  .state-icon {
    display: grid;
    place-items: center;
    width: 48px;
    height: 48px;
    margin-bottom: var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
    color: var(--brand);
    font: 750 var(--type-lg) var(--font-mono);
  }
  .state-icon :global(svg) {
    width: 22px;
    height: 22px;
  }
  .archive-state h2 {
    font-size: var(--type-lg);
  }
  .archive-state p {
    max-width: 420px;
    margin-top: 6px;
    color: var(--text-muted);
    font-size: var(--type-sm);
    line-height: 1.5;
  }
  .archive-state button {
    margin-top: var(--space-4);
    border-color: color-mix(in srgb, var(--brand) 55%, var(--border));
    color: var(--brand-strong);
  }
  .empty-management {
    margin-top: var(--space-4);
  }
  .archive-state.error .state-icon {
    color: var(--err);
  }
  .archive-overview {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-5);
    padding: 11px var(--space-4);
    border-bottom: 1px solid var(--border);
    background:
      linear-gradient(180deg, var(--surface-2), var(--surface-1) 82%),
      var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  .archive-overview > div {
    min-width: 0;
  }
  .archive-overview span {
    display: block;
    color: var(--text-muted);
    font: 700 9px var(--font-mono);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .archive-overview strong {
    display: block;
    overflow-wrap: anywhere;
    font: 650 var(--type-xs) var(--font-mono);
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
    text-align: right;
  }
  .overview-dates strong {
    margin-top: 3px;
  }
  .archive-overview i {
    color: var(--text-muted);
    font-style: normal;
  }
  .archive-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: 8px var(--space-4);
    border-bottom: 1px solid var(--border);
  }
  .archive-toolbar > p {
    display: grid;
    gap: 2px;
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
  .workspace-body.wide-layout .archive-list {
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior-y: contain;
  }
  .workspace-body.with-side .archive-list {
    border-right: 1px solid var(--border-strong);
  }
  .column-head,
  .result-row {
    display: grid;
    grid-template-columns: minmax(150px, 1.25fr) repeat(
        var(--metric-columns),
        minmax(82px, 1fr)
      );
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
    background:
      linear-gradient(180deg, var(--surface-2), var(--surface-1)),
      var(--surface-1);
    background-clip: padding-box;
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
    border: 0;
    background: transparent;
    color: var(--text-muted);
    font: 700 9px var(--font-mono);
    letter-spacing: 0.05em;
    text-transform: uppercase;
    cursor: pointer;
  }
  .column-head > span:first-child button {
    justify-content: flex-start;
  }
  .column-head button:hover {
    background: var(--brand-soft);
    color: var(--text);
  }
  .column-head [aria-sort]:not([aria-sort="none"]) button {
    color: var(--brand-strong);
  }
  .column-head i {
    position: relative;
    width: 9px;
    height: 12px;
    flex: none;
    color: var(--brand-strong);
  }
  .column-head i::after {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 4px;
    height: 4px;
    border: solid currentColor;
    border-width: 0 1.5px 1.5px 0;
    content: "";
    opacity: 0;
    transform: rotate(45deg);
    transition: opacity var(--dur-hover) var(--ease-out);
  }
  .column-head button:hover i::after {
    opacity: 0.35;
  }
  .column-head [aria-sort="descending"] i::after,
  .column-head [aria-sort="ascending"] i::after {
    opacity: 1;
  }
  .column-head [aria-sort="ascending"] i::after {
    top: 5px;
    transform: rotate(225deg);
  }
  .head-icon {
    display: grid;
    color: var(--tone, var(--text-soft));
  }
  .head-icon :global(svg) {
    width: 13px;
    height: 13px;
  }
  [data-tone="download"] {
    --tone: var(--phase-download);
  }
  [data-tone="upload"] {
    --tone: var(--phase-upload);
  }
  [data-tone="bidirectional"] {
    --tone: var(--phase-bidirectional);
  }
  [data-tone="idle"],
  [data-tone="loaded"] {
    --tone: var(--phase-latency);
  }
  ol {
    margin: 0;
    padding: 0 var(--space-2) var(--space-2);
    list-style: none;
  }
  li {
    min-width: 0;
    border-bottom: 1px solid var(--border-subtle);
    content-visibility: auto;
    contain-intrinsic-size: 58px;
  }
  li.selected {
    content-visibility: visible;
  }
  .result-row {
    position: relative;
    min-height: 56px;
    color: inherit;
    text-decoration: none;
    transition:
      background var(--dur-hover) var(--ease-out),
      box-shadow var(--dur-hover) var(--ease-out);
  }
  .result-row:hover {
    background: var(--surface-2);
  }
  .result-row[aria-current="true"] {
    background: color-mix(in srgb, var(--brand-soft) 62%, var(--surface-1));
    box-shadow:
      inset 3px 0 0 var(--brand),
      inset 0 1px 0 color-mix(in srgb, var(--brand) 22%, transparent),
      inset 0 -1px 0 color-mix(in srgb, var(--brand) 22%, transparent);
  }
  .date-cell,
  .metric-cell {
    min-width: 0;
    padding: 10px;
  }
  .date-cell {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .date-cell time {
    min-width: 0;
    flex: 1;
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
    font: 500 9px var(--font-mono);
  }
  .row-badges {
    display: grid;
    justify-items: end;
    gap: 3px;
  }
  .row-badges em {
    padding: 2px 5px;
    border: 1px solid var(--border);
    border-radius: var(--r-full);
    font: 700 8px var(--font-mono);
    font-style: normal;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .row-badges .partial {
    border-color: color-mix(in srgb, var(--warn) 45%, var(--border));
    background: var(--warn-soft);
    color: var(--warn);
  }
  .row-badges .selected-badge {
    border-color: color-mix(in srgb, var(--brand) 48%, var(--border));
    background: var(--brand-soft);
    color: var(--brand-strong);
  }
  .metric-cell {
    display: grid;
    align-content: center;
    text-align: right;
  }
  .metric-cell small {
    display: none;
  }
  .metric-cell strong {
    overflow-wrap: anywhere;
    color: var(--text);
    font: 620 11px var(--font-mono);
    line-height: 1.35;
  }
  .detail-inspector {
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior-y: contain;
    background: var(--surface-1);
  }
  .inline-inspector {
    border-top: 2px solid color-mix(in srgb, var(--brand) 50%, var(--border));
    background: var(--surface-1);
  }
  .selection-state {
    display: grid;
    justify-items: start;
    align-content: start;
    gap: 7px;
    padding: var(--space-5);
    border-top: 1px solid var(--border-strong);
    background: var(--surface-1);
  }
  .selection-state.side-state {
    position: sticky;
    top: 0;
    border-top: 0;
  }
  .selection-state > span {
    color: var(--warn);
    font: 750 var(--type-lg) var(--font-mono);
  }
  .selection-state h2 {
    font-size: var(--type-md);
  }
  .selection-state p {
    color: var(--text-muted);
    font-size: var(--type-sm);
    line-height: 1.45;
  }
  .selection-state button {
    margin-top: var(--space-2);
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
    font: 600 9px var(--font-mono);
  }
  .sr-status {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
  }
  @container (max-width: 820px) {
    .column-head {
      display: none;
    }
    ol {
      display: grid;
      gap: 6px;
      padding: 8px;
    }
    li {
      border: 1px solid var(--border);
      border-radius: var(--r-chrome);
      background:
        linear-gradient(180deg, var(--surface-2), transparent), var(--surface-1);
      box-shadow: var(--elev-tile);
      contain-intrinsic-size: 76px;
    }
    li.selected {
      border-color: color-mix(in srgb, var(--brand) 64%, var(--border));
      box-shadow:
        0 0 0 2px color-mix(in srgb, var(--brand) 16%, transparent),
        var(--elev-tile);
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
      gap: 8px;
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
      gap: 4px;
    }
    .metrics-row {
      display: grid;
      grid-template-columns: repeat(var(--metric-columns), minmax(0, 1fr));
      min-width: 0;
    }
    .metric-cell {
      gap: 3px;
      min-width: 0;
      padding: 6px 7px 7px;
      border-left: 1px solid var(--border-subtle);
      text-align: left;
    }
    .metric-cell:first-child {
      border-left: 0;
    }
    .metric-cell small {
      display: flex;
      align-items: center;
      gap: 5px;
      color: var(--text-muted);
      font: 700 9px var(--font-mono);
      letter-spacing: 0.05em;
      text-transform: uppercase;
    }
    .metric-cell small span {
      display: grid;
      color: var(--tone, var(--text-soft));
    }
    .metric-cell small :global(svg) {
      width: 12px;
      height: 12px;
    }
    .metric-cell strong {
      overflow: hidden;
      font-size: 9px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
  }
  @container (max-width: 560px) {
    .history-head {
      padding: 11px var(--space-3);
    }
    .close-history strong {
      display: none;
    }
    .close-history {
      width: 32px;
      padding: 0;
      justify-content: center;
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
    .saving-notice p {
      line-height: 1.4;
    }
  }
  @container (max-width: 330px) {
    .archive-toolbar {
      display: grid;
    }
    .toolbar-actions {
      justify-content: space-between;
    }
    .date-cell time {
      gap: 5px;
    }
    .date-cell time small {
      font-size: 8px;
    }
    .metric-cell {
      padding-inline: 5px;
    }
    .metric-cell small {
      gap: 3px;
      font-size: 8px;
      letter-spacing: 0.02em;
    }
    .metric-cell small :global(svg) {
      width: 10px;
      height: 10px;
    }
    .metric-cell strong {
      font-size: 9px;
    }
  }
  @media (prefers-reduced-motion: no-preference) {
    .history-workspace {
      animation: reveal-history var(--dur-enter) var(--ease-out) both;
    }
    .inline-inspector,
    .detail-inspector {
      animation: reveal-detail var(--dur-enter) var(--ease-out) both;
    }
    @keyframes reveal-history {
      from {
        transform: translateY(4px) scale(0.997);
      }
    }
    @keyframes reveal-detail {
      from {
        transform: translateY(4px);
      }
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .history-workspace,
    .result-row {
      animation: none;
      transition: none;
    }
    .column-head i::after {
      transition: none;
    }
  }
  @media (max-width: 759px) and (orientation: portrait) {
    .history-workspace {
      flex: none;
      height: auto;
      min-height: calc(100svh - var(--topbar-h) - var(--statusbar-h) - 24px);
      overflow: visible;
    }
    .workspace-body {
      flex: none;
      overflow: visible;
      overscroll-behavior: auto;
    }
  }
</style>
