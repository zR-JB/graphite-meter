<script lang="ts">
  import { onMount, tick } from "svelte";
  import { ICON } from "../constants";
  import {
    broadcastHistory,
    historyChanges,
    HistoryRepository,
  } from "../history/repository";
  import {
    formatHistoryRate,
    formatLatency,
    formatRecentCompletion,
    stageStatusLabel,
  } from "../history/format";
  import {
    HISTORY_SORT_LABEL,
    naturalDescending,
    sortHistory,
    type HistorySort,
  } from "../history/sort";
  import type { HistoryRecordV1, StageStatus } from "../history/types";
  import type { HistoryColumn } from "../state/persistence";
  import { store } from "../state/store.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
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
  let records = $state<HistoryRecordV1[]>([]);
  let malformedCount = $state(0);
  let selectedState = $state<"ready" | "missing" | "malformed">("missing");
  let sort = $state<HistorySort>("date");
  let descending = $state(true);
  let visibleCount = $state(50);
  let renderedAt = $state(Date.now());
  let workspaceWidth = $state(0);
  let detailHeading = $state<HTMLElement>();
  let confirm = $state<
    { kind: "delete"; id: string } | { kind: "clear" } | null
  >(null);
  let actionError = $state("");
  let announcement = $state("");
  let focusedHeading: HTMLElement | null = null;
  let loadGeneration = 0;

  const columns = $derived(store.historyColumns);
  const ordered = $derived(sortHistory(records, sort, descending));
  const visibleRecords = $derived(ordered.slice(0, visibleCount));
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
    { label: string; short: string; icon: string; sort: HistorySort | null }
  > = {
    download: {
      label: "Download",
      short: "Down",
      icon: ICON.download,
      sort: "download",
    },
    upload: {
      label: "Upload",
      short: "Up",
      icon: ICON.upload,
      sort: "upload",
    },
    bidirectional: {
      label: "Bidirectional",
      short: "Bi-dir",
      icon: ICON.bidirectional,
      sort: "bidirectional",
    },
    idle: {
      label: "Idle latency",
      short: "Idle",
      icon: ICON.ping,
      sort: "idle",
    },
    loaded: {
      label: "Loaded latency",
      short: "Loaded",
      icon: ICON.ping,
      sort: "loaded",
    },
    status: {
      label: "Status",
      short: "Status",
      icon: ICON.check,
      sort: null,
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
      if (entry.status === "ready") records = [entry.record, ...records];
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

  function select(record: HistoryRecordV1) {
    focusedHeading = null;
    onNavigate(record.id);
  }

  function closeDetail() {
    const id = selectedId;
    onNavigate(null);
    window.setTimeout(() => {
      if (!id) return;
      document.querySelector<HTMLElement>(`[data-history-id="${id}"]`)?.focus();
    }, 0);
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
    confirm = null;
    if (!action) return;
    actionError = "";
    try {
      if (action.kind === "clear") {
        await repository.clear();
        records = [];
        malformedCount = 0;
        announcement = "History cleared.";
        if (selectedId) onNavigate(null);
        broadcastHistory({ type: "clear" });
      } else {
        await repository.delete(action.id);
        records = records.filter((record) => record.id !== action.id);
        announcement = "Result deleted.";
        if (selectedId === action.id) onNavigate(null);
        broadcastHistory({ type: "delete", id: action.id });
      }
      window.dispatchEvent(new Event("graphite-meter-history-changed"));
    } catch {
      actionError = "The local archive could not be changed. Try again.";
    }
  }

  function partial(record: HistoryRecordV1): boolean {
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

  function bidiRate(record: HistoryRecordV1): string {
    const result = record.stages.bidirectional;
    return result.down && result.up
      ? rate(result.down.reportedBytesPerSec + result.up.reportedBytesPerSec)
      : result.status === "partial"
        ? "One lane"
        : stageStatusLabel(result.status);
  }

  function loadedMetric(record: HistoryRecordV1): string {
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

  function metric(record: HistoryRecordV1, column: HistoryColumn): string {
    if (column === "download")
      return resultRate(
        record.stages.download.status,
        record.stages.download.result?.reportedBytesPerSec,
      );
    if (column === "upload")
      return resultRate(
        record.stages.upload.status,
        record.stages.upload.result?.reportedBytesPerSec,
      );
    if (column === "bidirectional") return bidiRate(record);
    if (column === "idle")
      return record.stages.latency.result
        ? formatLatency(record.stages.latency.result.reportedMs)
        : stageStatusLabel(record.stages.latency.status);
    if (column === "loaded") return loadedMetric(record);
    return partial(record) ? "Partial" : "Complete";
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

  function rowDate(value: number): string {
    return formatRecentCompletion(value, renderedAt) ?? dateLabel(value);
  }

  $effect(() => {
    const id = selectedId;
    if (loadState !== "ready") return;
    void resolveSelection(id, loadGeneration);
    if (!id) focusedHeading = null;
  });

  $effect(() => {
    const id = selectedRecord?.id;
    const target = detailHeading;
    if (!id || !target || target === focusedHeading) return;
    focusedHeading = target;
    void tick().then(() => target.focus({ preventScroll: sideInspector }));
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
    const refresh = () => void load(false);
    const refreshWhenVisible = () => {
      if (document.visibilityState === "visible") refresh();
    };
    const stopChanges = historyChanges(refresh);
    window.addEventListener("graphite-meter-history-changed", refresh);
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refreshWhenVisible);
    return () => {
      stopChanges();
      window.removeEventListener("graphite-meter-history-changed", refresh);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refreshWhenVisible);
      repository.close();
    };
  });
</script>

<section
  class="history-workspace"
  bind:clientWidth={workspaceWidth}
  aria-labelledby="history-title"
>
  <header class="history-head">
    <div class="history-title">
      <span class="archive-mark">{@html ICON.history}</span>
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
      <p>
        {store.historyWarning ||
          actionError ||
          `${malformedCount} malformed ${malformedCount === 1 ? "record was" : "records were"} ignored.`}
      </p>
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
    <div class="archive-state empty">
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
    </div>
  {:else}
    <div class="archive-overview" aria-label="History overview">
      <div class="overview-primary">
        <span>Saved locally</span><strong>{records.length}</strong>
      </div>
      <div>
        <span>Date span</span>
        <strong
          >{oldest == null ? "—" : dateLabel(oldest)} <i>to</i>
          {newest == null ? "—" : dateLabel(newest)}</strong
        >
      </div>
      <div class="overview-context">
        <span>View</span><strong
          >{columns.length + 1} columns · {HISTORY_SORT_LABEL[sort]}
          {descending ? "↓" : "↑"}</strong
        >
      </div>
    </div>

    <div class="archive-toolbar">
      <p>
        <strong>Results</strong><span
          >Newest completion is the default order.</span
        >
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
              <span>Date</span><i aria-hidden="true"
                >{sort === "date" ? (descending ? "↓" : "↑") : "↕"}</i
              >
            </button>
          </span>
          {#each columns as column}
            <span
              role="columnheader"
              data-tone={column}
              aria-sort={columnMeta[column].sort &&
              sort === columnMeta[column].sort
                ? descending
                  ? "descending"
                  : "ascending"
                : columnMeta[column].sort
                  ? "none"
                  : undefined}
            >
              {#if columnMeta[column].sort}
                <button
                  type="button"
                  onclick={() => sortColumn(columnMeta[column].sort!)}
                >
                  <span class="head-icon">{@html columnMeta[column].icon}</span>
                  <span>{columnMeta[column].short}</span>
                  <i aria-hidden="true"
                    >{sort === columnMeta[column].sort
                      ? descending
                        ? "↓"
                        : "↑"
                      : "↕"}</i
                  >
                </button>
              {:else}
                <span class="static-head"
                  ><span class="head-icon">{@html columnMeta[column].icon}</span
                  >{columnMeta[column].short}</span
                >
              {/if}
            </span>
          {/each}
        </div>
        <ol>
          {#each visibleRecords as record (record.id)}
            <li class:selected={selectedId === record.id}>
              <a
                class="result-row"
                data-history-id={record.id}
                style={`--metric-columns:${columns.length}`}
                href={`#/history/${record.id}`}
                aria-current={selectedId === record.id ? "true" : undefined}
                aria-expanded={selectedId === record.id}
                aria-label={`${fullDate(record.completedAt)}${partial(record) ? ", partial result" : ", complete result"}. Download ${resultRate(record.stages.download.status, record.stages.download.result?.reportedBytesPerSec)}. Upload ${resultRate(record.stages.upload.status, record.stages.upload.result?.reportedBytesPerSec)}. Bidirectional ${bidiRate(record)}. Idle ${record.stages.latency.result ? formatLatency(record.stages.latency.result.reportedMs) : stageStatusLabel(record.stages.latency.status)}. Loaded ${loadedMetric(record)}.`}
                onclick={(event) => {
                  event.preventDefault();
                  select(record);
                }}
              >
                <span class="date-cell">
                  <span class="date-mark" aria-hidden="true"></span>
                  <time datetime={new Date(record.completedAt).toISOString()}>
                    <strong title={fullDate(record.completedAt)}
                      >{rowDate(record.completedAt)}</strong
                    >
                    <small
                      >{formatRecentCompletion(record.completedAt, renderedAt)
                        ? dateLabel(record.completedAt)
                        : new Date(record.completedAt).toLocaleTimeString(
                            undefined,
                            { hour: "2-digit", minute: "2-digit" },
                          )}</small
                    >
                  </time>
                  <span class="row-badges">
                    {#if partial(record) && !columns.includes("status")}<em
                        class="partial">Partial</em
                      >{/if}
                    {#if selectedId === record.id}<em class="selected-badge"
                        >Selected</em
                      >{/if}
                  </span>
                </span>
                {#each columns as column}
                  <span class="metric-cell" data-tone={column}>
                    <small
                      ><span>{@html columnMeta[column].icon}</span>{columnMeta[
                        column
                      ].short}</small
                    >
                    <strong
                      class:partial-value={column === "status" &&
                        partial(record)}>{metric(record, column)}</strong
                    >
                  </span>
                {/each}
              </a>
              {#if selectedId === record.id && selectedRecord && !sideInspector}
                <div class="inline-inspector">
                  <HistoryResultDetail
                    record={selectedRecord}
                    onClose={closeDetail}
                    onDelete={() =>
                      (confirm = { kind: "delete", id: record.id })}
                    bind:heading={detailHeading}
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
        {:else}
          <footer class="archive-management">
            <div>
              <strong>Archive management</strong>
              <span>Destructive actions affect this browser only.</span>
            </div>
            <button type="button" onclick={() => (confirm = { kind: "clear" })}
              >Clear all saved results</button
            >
          </footer>
        {/if}
      </div>

      {#if sideInspector && selectedRecord}
        <aside class="detail-inspector" aria-label="Selected result">
          <HistoryResultDetail
            record={selectedRecord}
            onClose={closeDetail}
            onDelete={() =>
              (confirm = { kind: "delete", id: selectedRecord.id })}
            bind:heading={detailHeading}
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
              ? "This record failed the local schema checks and was not rendered."
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
  title={confirm?.kind === "clear"
    ? "Clear result history?"
    : "Delete this result?"}
  description={confirm?.kind === "clear"
    ? `Permanently remove all ${records.length} saved results from this browser?`
    : "Permanently remove this saved result from this browser?"}
  confirmLabel={confirm?.kind === "clear" ? "Clear history" : "Delete result"}
  onCancel={() => (confirm = null)}
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
    background: linear-gradient(
      110deg,
      color-mix(in srgb, var(--brand) 8%, var(--surface-2)),
      var(--surface-1) 58%
    );
  }
  .history-title {
    display: flex;
    align-items: center;
    gap: 9px;
    min-width: 0;
  }
  .archive-mark {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    flex: none;
    border: 1px solid color-mix(in srgb, var(--brand) 48%, var(--border));
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
    color: var(--brand-strong);
  }
  .archive-mark :global(svg) {
    width: 17px;
    height: 17px;
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
    color: var(--text-soft);
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
  .archive-state.error .state-icon {
    color: var(--err);
  }
  .archive-overview {
    display: grid;
    grid-template-columns:
      minmax(100px, 0.55fr) minmax(250px, 1.4fr)
      minmax(180px, 0.8fr);
    border-bottom: 1px solid var(--border);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
  }
  .archive-overview > div {
    min-width: 0;
    padding: 11px var(--space-4);
  }
  .archive-overview > div + div {
    border-left: 1px solid var(--border);
  }
  .archive-overview span {
    display: block;
    color: var(--text-soft);
    font: 700 9px var(--font-mono);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .archive-overview strong {
    display: block;
    margin-top: 4px;
    overflow-wrap: anywhere;
    font: 650 var(--type-xs) var(--font-mono);
  }
  .archive-overview .overview-primary strong {
    color: var(--brand-strong);
    font-size: var(--type-md);
    line-height: 0.9;
  }
  .archive-overview i {
    color: var(--text-soft);
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
  .archive-toolbar > p span {
    color: var(--text-soft);
    font-size: 9px;
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
  .column-head {
    position: sticky;
    top: 0;
    z-index: 5;
    border-bottom: 1px solid var(--border-strong);
    background: var(--surface-2);
    background-clip: padding-box;
    box-shadow: var(--elev-tile);
  }
  .column-head > span {
    min-width: 0;
  }
  .column-head > span + span {
    border-left: 1px solid var(--border-subtle);
  }
  .column-head button,
  .static-head {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 6px;
    width: 100%;
    min-height: 34px;
    padding: 0 10px;
    border: 0;
    background: transparent;
    color: var(--text-soft);
    font: 700 9px var(--font-mono);
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .column-head > span:first-child button {
    justify-content: flex-start;
  }
  .column-head button {
    cursor: pointer;
  }
  .column-head button:hover {
    background: var(--brand-soft);
    color: var(--text);
  }
  .column-head [aria-sort]:not([aria-sort="none"]) button {
    color: var(--brand-strong);
  }
  .column-head i {
    color: var(--brand-strong);
    font: 750 12px var(--font-mono);
    font-style: normal;
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
    padding: 0;
    list-style: none;
  }
  li {
    min-width: 0;
    content-visibility: auto;
    contain-intrinsic-size: 58px;
  }
  li + li {
    border-top: 1px solid var(--border);
  }
  li.selected {
    content-visibility: visible;
  }
  .result-row {
    position: relative;
    color: inherit;
    text-decoration: none;
    transition: background var(--dur-hover) var(--ease-out);
  }
  .result-row::before {
    content: "";
    position: absolute;
    inset: 8px auto 8px 0;
    width: 3px;
    border-radius: 0 2px 2px 0;
    background: transparent;
    transition: background var(--dur-hover) var(--ease-out);
  }
  .result-row:hover {
    background: color-mix(in srgb, var(--brand) 5%, var(--surface-2));
  }
  .result-row[aria-current="true"] {
    background: color-mix(in srgb, var(--brand) 12%, var(--surface-1));
    box-shadow: inset 0 0 0 1px
      color-mix(in srgb, var(--brand) 40%, transparent);
  }
  .result-row[aria-current="true"]::before {
    background: var(--brand);
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
  .date-mark {
    width: 6px;
    height: 6px;
    flex: none;
    border: 1px solid var(--brand);
    border-radius: var(--r-full);
    background: var(--surface-1);
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
    color: var(--text-soft);
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
    border-left: 1px solid var(--border-subtle);
    text-align: right;
  }
  .metric-cell small {
    display: none;
  }
  .metric-cell strong {
    overflow-wrap: anywhere;
    color: var(--text);
    font: 620 10px var(--font-mono);
    line-height: 1.35;
  }
  .metric-cell strong.partial-value {
    color: var(--warn);
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
  .inline-inspector :global(.detail-head) {
    position: relative;
    top: auto;
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
    color: var(--text-soft);
    font: 600 9px var(--font-mono);
  }
  .archive-management {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    margin: var(--space-5) var(--space-4) var(--space-4);
    padding-top: var(--space-4);
    border-top: 1px solid var(--border);
    color: var(--text-soft);
  }
  .archive-management div {
    display: grid;
    gap: 3px;
  }
  .archive-management strong {
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .archive-management span {
    font-size: 9px;
  }
  .archive-management button {
    min-height: 30px;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: transparent;
    color: var(--text-soft);
    font-size: var(--type-xs);
    cursor: pointer;
  }
  .archive-management button:hover {
    border-color: var(--err);
    color: var(--err);
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
    .archive-overview {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .archive-overview > div:nth-child(3) {
      border-left: 0;
      border-top: 1px solid var(--border);
    }
    .column-head {
      display: none;
    }
    ol {
      display: grid;
      gap: var(--space-2);
      padding: var(--space-3);
    }
    li {
      border: 1px solid var(--border);
      border-radius: var(--r-chrome);
      background: var(--surface-inset);
      box-shadow: var(--elev-recess);
      contain-intrinsic-size: 190px;
    }
    li + li {
      border-top: 1px solid var(--border);
    }
    li.selected {
      border-color: color-mix(in srgb, var(--brand) 64%, var(--border));
      box-shadow:
        0 0 0 2px color-mix(in srgb, var(--brand) 16%, transparent),
        var(--elev-recess);
    }
    .result-row {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .date-cell {
      grid-column: 1 / -1;
      padding: 11px 12px;
      border-bottom: 1px solid var(--border);
      background: var(--surface-2);
    }
    .metric-cell {
      gap: 4px;
      padding: 9px 12px;
      border-left: 0;
      text-align: left;
    }
    .metric-cell:nth-of-type(even) {
      border-left: 1px solid var(--border-subtle);
    }
    .metric-cell:nth-of-type(n + 4) {
      border-top: 1px solid var(--border-subtle);
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
      font-size: var(--type-xs);
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
      grid-template-columns: minmax(88px, 0.65fr) minmax(0, 1.35fr);
    }
    .overview-context {
      display: none;
    }
    .archive-toolbar {
      align-items: flex-start;
      padding-inline: var(--space-3);
    }
    .archive-toolbar > p span {
      display: none;
    }
    .saving-notice {
      align-items: flex-start;
    }
    .saving-notice p {
      line-height: 1.4;
    }
    .archive-management {
      align-items: flex-start;
      margin-inline: var(--space-3);
    }
  }
  @container (max-width: 330px) {
    .archive-mark {
      width: 32px;
      height: 32px;
    }
    .archive-overview > div {
      padding: 9px var(--space-3);
    }
    .archive-toolbar {
      display: grid;
    }
    .toolbar-actions {
      justify-content: space-between;
    }
    .result-row {
      grid-template-columns: 1fr;
    }
    .metric-cell:nth-of-type(even) {
      border-left: 0;
    }
    .metric-cell:nth-of-type(n + 3) {
      border-top: 1px solid var(--border-subtle);
    }
  }
  @media (prefers-reduced-motion: no-preference) {
    .inline-inspector,
    .detail-inspector {
      animation: reveal-detail var(--dur-enter) var(--ease-out) both;
    }
    @keyframes reveal-detail {
      from {
        opacity: 0;
        transform: translateY(4px);
      }
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
