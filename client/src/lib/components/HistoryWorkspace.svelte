<script lang="ts" module>
  // Dismissal lasts the page's lifetime and returns when the count changes.
  let dismissedMalformed = $state(0);
  // Date formatting is shared across the archive's rows and repeated visits.
  const dates = {
    exact: new Intl.DateTimeFormat(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }),
    time: new Intl.DateTimeFormat(undefined, {
      hour: "2-digit",
      minute: "2-digit",
    }),
    short: new Intl.DateTimeFormat(undefined, {
      month: "short",
      day: "numeric",
    }),
    full: new Intl.DateTimeFormat(undefined, {
      month: "short",
      day: "numeric",
      year: "numeric",
    }),
    month: new Intl.DateTimeFormat(undefined, {
      month: "long",
      year: "numeric",
    }),
  };
</script>

<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { IconName } from "../presentation/icons";
  import { onMount, tick, untrack } from "svelte";
  import { resize } from "../actions/resize";
  import { tooltip } from "../actions/tooltip";
  import { nextFrame, wallNow } from "../presentation/motion.svelte";
  import { canFocus, hasFocus, activeModal } from "../actions/focus";
  import { createUuid } from "../uuid";
  import {
    announceHistoryChanged,
    HistoryRepository,
    onHistoryChanged,
  } from "../history/repository";
  import { formatRecentCompletion } from "../history/format";
  import {
    fmtAddedMs,
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
  import {
    DEFAULT_HISTORY_SPLIT,
    type HistoryColumn,
  } from "../state/persistence";
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
  // Beside a result the list keeps its share of the width; these mirror the CSS clamp for the handle.
  const MIN_LIST_WIDTH = 360;
  const MIN_DETAIL_WIDTH = 460;
  let bodyWidth = $state(0);
  const maxListWidth = $derived(bodyWidth - MIN_DETAIL_WIDTH);
  const listWidth = $derived(
    Math.round(
      Math.max(
        MIN_LIST_WIDTH,
        Math.min(maxListWidth, store.historySplit * bodyWidth),
      ),
    ),
  );
  let previousSelectedId: string | null = null;
  let confirm = $state<
    { kind: "delete"; id: string } | { kind: "clear" } | null
  >(null);
  let confirmInvoker = $state<HTMLElement | null>(null);
  let actionError = $state("");
  let loadGeneration = 0;
  let loadController: AbortController | undefined;

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
    return `${sameYear ? dates.short.format(first) : dateLabel(first)} – ${dateLabel(last)}`;
  });

  // A sorted head's name is its text and its order, with no stray space before the comma.
  const headText = (column: HistorySort) =>
    column === "date"
      ? "Date"
      : `${COLUMN[column].short} ${scales[column].unit}`.trim();
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
    loadController?.abort();
    const controller = (loadController = new AbortController());
    if (showLoading) loadState = "loading";
    actionError = "";
    try {
      const result = await repository.listWithDiagnostics(controller.signal);
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

  // A hidden column cannot order the list; Date takes over.
  $effect(() => {
    if (sort !== "date" && !columns.includes(sort)) setSort("date", true);
  });

  // Rows fold to the compact layout once the table's columns no longer fit their content;
  // reading the rows and columns measures again whenever they change.
  function fitTable(node: HTMLElement) {
    void [groups, columns];
    let alive = true;
    let stop: (() => void) | null = null;
    const measure = () => {
      stop = null;
      node.classList.remove("compact");
      node.classList.toggle("compact", node.scrollWidth > node.clientWidth);
    };
    const fit = () => {
      if (alive && !stop) stop = nextFrame(measure);
    };
    const observer = new ResizeObserver(fit);
    observer.observe(node);
    void document.fonts.ready.then(fit);
    return () => {
      alive = false;
      observer.disconnect();
      stop?.();
    };
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

  // Under each rate, the latency its load added; under idle, its jitter; under loaded, whose load it was.
  function note(record: HistoryRecord, column: HistoryColumn): string {
    const { addedLatency, latency, latencyByStage } = record.result;
    if (column === "idle")
      return latency?.jitterMs == null
        ? ""
        : `jitter ${fmtMs(latency.jitterMs)}`;
    if (column === "loaded") {
      const worst = (["download", "upload", "bidirectional"] as const)
        .filter((stage) => latencyByStage[stage]?.p50Ms != null)
        .toSorted(
          (a, b) => latencyByStage[b]!.p50Ms! - latencyByStage[a]!.p50Ms!,
        )[0];
      return worst ? STAGE[worst].short.toLowerCase() : "";
    }
    const added = addedLatency?.[column];
    return added == null ? "" : `${fmtAddedMs(added)} ms`;
  }
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
        note: note(record, column),
      };
    const missing = (text: string) => ({ text, share: null, note: "" });
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
    const exact = dates.exact.format(record.completedAt);
    const { outcome, multiServer } = record.result;
    const metrics = columns.map((column) => metric(record, column));
    const servers = multiServer.selection;
    const where =
      servers.length > 1
        ? `${servers.length} servers`
        : (servers[0]?.name ?? "");
    const time = dates.time.format(record.completedAt);
    const recentDay = ["Today", "Yesterday"].includes(
      groupHeading(record.completedAt),
    );
    const day = dates.short.format(record.completedAt);
    return {
      record,
      exact,
      primary: !byDay
        ? `${dateLabel(record.completedAt)}, ${time}`
        : recentDay
          ? time
          : `${day}, ${time}`,
      secondary: [where, recent].filter(Boolean).join(", "),
      outcome,
      metrics,
      label: [
        [exact, where, `${OUTCOME[outcome].toLowerCase()} result`]
          .filter(Boolean)
          .join(", "),
        ...columns.map((column, index) => {
          const { text, share, note } = metrics[index];
          const value = `${HISTORY_SORT_LABEL[column]} ${text}${share === null ? "" : ` ${scales[column].unit}`}`;
          if (!note) return value;
          return `${value}, ${column === "idle" ? `${note} ms` : column === "loaded" ? `under ${note}` : `${note} added`}`;
        }),
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
    return dates.month.format(value);
  }

  function dateLabel(value: number): string {
    return dates.full.format(value);
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
      loadController?.abort();
      stopChanges();
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
  <header class="sheet-head history-head">
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
              <span><strong>Clear history</strong></span>
            </button>
          {/snippet}
        </MoreMenu>
      {/if}
      <button
        class="btn btn-icon btn-quiet close-history"
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
          <span class="notice-actions">
            <button
              class="btn"
              type="button"
              onclick={async () => {
                actionError = "";
                try {
                  const removed = await repository.removeMalformed();
                  malformedCount = 0;
                  announce(
                    `${removed} malformed ${removed === 1 ? "record" : "records"} removed.`,
                  );
                  announceHistoryChanged(changeSource);
                } catch {
                  actionError = "Unable to remove the malformed records.";
                }
                workspace?.focus({ preventScroll: true });
              }}>Remove them</button
            >
            <button
              class="btn btn-quiet"
              type="button"
              onclick={() => {
                dismissedMalformed = malformedCount;
                workspace?.focus({ preventScroll: true });
              }}>Dismiss</button
            >
          </span>
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
          Saving is paused. Resume it to keep future results in this browser.
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
    <div
      class="workspace-body"
      class:has-detail={selectedId !== null}
      style:--split={store.historySplit}
      bind:clientWidth={bodyWidth}
    >
      <div class="history-list" bind:this={list} {@attach fitTable}>
        <div class="history-table" style:--metric-columns={columns.length}>
          <div class="column-head page-fill" role="group" aria-label="Sort by">
            {#each ["date" as const, ...columns] as column (column)}
              <button
                type="button"
                aria-label={sort === column
                  ? `${headText(column)}, ${descending ? "descending" : "ascending"}`
                  : undefined}
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
                <!-- The unit is part of the explained word, so a tip that flips below never covers it. -->
                <span
                  {@attach column === "date"
                    ? null
                    : tooltip(() => COLUMN[column].help)}
                  >{column === "date"
                    ? "Date"
                    : COLUMN[column]
                        .short}{#if column !== "date" && scales[column].unit}<span
                      class="unit">{scales[column].unit}</span
                    >{/if}</span
                >
                <i aria-hidden="true"></i>
              </button>
              {#if column === "date"}<span class="outcome-head"></span>{/if}
            {/each}
          </div>
          {#each groups as group, index (group.records[0].id)}
            <section class="group day">
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
                        class="result-row link-row"
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
                              class="status-dot inline"
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
                            {#if cell.note}<small class="note"
                                >{cell.note}</small
                              >{/if}
                          </span>
                        {/each}
                      </a>
                      {#snippet failed()}
                        <a
                          class="result-row link-row"
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
                            ><span class="status-dot inline" data-tone="warn"
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
      {#if selectedId !== null}
        <div
          class="resize-handle"
          role="slider"
          aria-orientation="horizontal"
          aria-label="Resize results list (arrow keys; Enter to reset)"
          aria-valuemin={MIN_LIST_WIDTH}
          aria-valuemax={maxListWidth}
          aria-valuenow={listWidth}
          aria-valuetext={`${listWidth} pixels wide`}
          tabindex="0"
          {@attach resize({
            side: "left",
            width: () => listWidth,
            min: MIN_LIST_WIDTH,
            max: () => maxListWidth,
            set: (px) => store.prefer({ historySplit: px / bodyWidth }),
            reset: () => store.prefer({ historySplit: DEFAULT_HISTORY_SPLIT }),
          })}
        ></div>
      {/if}

      {#snippet unavailable(malformed: boolean)}
        <div class="detail-pane empty-state" role="status">
          <span class="empty-icon">!</span>
          <h2>
            {malformed ? "Unreadable saved result" : "Result not found"}
          </h2>
          <p>
            {malformed
              ? "This record uses an unsupported format or failed validation."
              : "This result is not in History."}
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
  title={confirm?.kind === "clear" ? "Clear history?" : "Delete this result?"}
  description={confirm?.kind === "clear"
    ? "All saved results are permanently removed from this browser."
    : "This saved result is permanently removed from this browser."}
  cancelLabel={confirm?.kind === "clear" ? "Keep history" : "Keep result"}
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
  /* Both panes scroll beneath the head, so its rule always shows. */
  .history-head {
    border-bottom-color: var(--border);
    animation: none;
  }
  /* The dot sits on the word's baseline, so a row keeps one line of text. */
  .history-workspace :global(.outcome) {
    display: inline-flex;
    align-items: baseline;
    gap: var(--space-2);
    color: var(--text-muted);
    font-size: var(--type-sm);
    white-space: nowrap;
  }
  /* Reserving the list's scrollbar gutter, a notice ends on the rows' edge. */
  .notices {
    display: grid;
    flex: none;
    gap: var(--space-1);
    padding: var(--space-3) var(--panel-pad) 0;
    overflow: hidden;
    scrollbar-gutter: stable;
  }
  .notice {
    align-items: center;
    justify-content: space-between;
  }
  .notice-actions {
    display: flex;
    gap: var(--space-2);
  }
  .workspace-body {
    position: relative;
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
    scrollbar-gutter: stable;
  }
  @supports (animation-timeline: scroll()) {
    .detail-pane {
      scroll-timeline: --sheet block;
    }
  }
  .has-detail .history-list {
    visibility: hidden;
  }
  /* On the hairline, reaching into the detail's margin so the list keeps its scrollbar. */
  .resize-handle {
    display: none;
    grid-area: 1 / 2;
    left: calc(var(--hairline) - 2px);
  }
  .resize-handle::after {
    left: 2px;
  }
  @container history (min-width: 821px) {
    /* The list keeps its share of the width; at any width both panes keep their minimum. */
    .has-detail {
      grid-template-columns:
        clamp(360px, calc(var(--split) * 100%), calc(100% - 460px))
        minmax(0, 1fr);
    }
    .has-detail .history-list {
      visibility: visible;
    }
    .detail-pane {
      grid-area: 1 / 2;
      border-left: var(--hairline) solid var(--border);
    }
    .resize-handle {
      display: block;
    }
  }
  .history-list {
    container: history-list / inline-size;
  }
  /* A reading table, not a spread: the time takes the slack and each value column is as wide as its content, so
     the figures sit together at the right, and the table stops at a width the eye crosses in one line. Columns
     never shrink below their content: the rows fold first (fitTable). */
  .history-table {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto repeat(
        var(--metric-columns),
        max-content
      );
    row-gap: var(--space-5);
    max-width: 1120px;
    padding: 0 var(--panel-pad) var(--space-6);
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
  /* Its rule comes in as rows pass beneath it, as a sheet's head's does. */
  .column-head {
    position: sticky;
    top: 0;
    z-index: 1;
    margin: 0 calc(-1 * var(--panel-pad)) calc(-1 * var(--space-2));
    padding: var(--space-3) var(--panel-pad) var(--space-1);
    border-bottom: var(--hairline) solid transparent;
  }
  @supports (animation-timeline: scroll()) {
    .history-list {
      scroll-timeline: --list block;
    }
    .column-head {
      animation: sheet-rule linear both;
      animation-timeline: --list;
      animation-range: 0 var(--space-3);
    }
  }
  .column-head button {
    position: relative;
    display: flex;
    align-items: flex-start;
    justify-content: flex-end;
    gap: 5px;
    min-height: var(--control-h);
    padding: 6px var(--space-3);
    border-radius: var(--r-well);
    color: var(--text-muted);
    font: var(--w-strong) var(--type-sm) / 1.3 var(--font-sans);
    text-align: end;
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
  @media (pointer: coarse) {
    .column-head button {
      min-height: var(--hit);
    }
  }
  .column-head [aria-pressed="true"] {
    color: var(--text);
  }
  /* The sort mark sits in the padding, so labels share their values' edge. */
  .column-head i {
    position: absolute;
    right: 3px;
    top: 12px;
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
  /* Centred on its word's line, however tall the row of heads is. */
  .column-head > button:first-child i {
    position: static;
    margin: calc((1.3em - 5px) / 2) 0 0 2px;
  }
  .column-head [data-order] i {
    opacity: 1;
  }
  .column-head [data-order="ascending"] i {
    rotate: 225deg;
  }
  /* One label line tall, so the icon centres on its word above the unit. */
  .head-icon {
    display: grid;
    flex: none;
    align-content: center;
    height: 1.3em;
    color: var(--tone);
  }
  .head-icon :global(svg) {
    width: var(--icon-sm);
    height: var(--icon-sm);
  }
  .unit {
    display: block;
    color: var(--text-soft);
    font: 500 var(--type-2xs) / 1.4 var(--font-mono);
  }
  /* A day spaces its heading as a .group, but its gap must not open the table's columns. */
  .day {
    column-gap: 0;
  }
  .day > h3 {
    grid-column: 1 / -1;
  }
  /* The list is page, not plate: a day's rows sit between two rules, with hairlines between them. */
  ol {
    border-block: var(--hairline) solid var(--border-subtle);
    font: var(--role-row);
  }
  li + li {
    border-top: var(--hairline) solid var(--border-subtle);
  }
  /* Its wash and focus ring sit 2 px inside the row (.link-row), which has no plate inset. */
  .result-row {
    --row-inset: 0px;
    align-items: first baseline;
    min-height: var(--row-h);
  }
  .result-row > * {
    padding: 7px var(--space-3);
  }
  /* The time over its server, on the lines of the values and their notes; a long name is cut, never wrapped.
     The time is the row's name, weighted like its values. */
  time {
    display: grid;
    font-weight: 500;
    white-space: nowrap;
  }
  time small {
    contain: inline-size;
    overflow: hidden;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1.3 var(--font-sans);
    text-overflow: ellipsis;
  }
  .outcome:empty {
    padding: 0;
  }
  /* A value, then what it cost or how it varied, in the column's own ink. */
  .metric {
    display: grid;
    grid-template: "bar value" auto ". note" auto / minmax(0, 4.5rem) auto;
    align-items: baseline;
    justify-content: end;
    column-gap: var(--space-2);
  }
  /* Deeper than --tone-ink, so a note keeps 4.5:1 on a row's wash as on the page. */
  .note {
    grid-area: note;
    justify-self: end;
    color: color-mix(in oklab, var(--tone) 65%, var(--text));
    font: var(--w-normal) var(--type-sm) / 1.3 var(--font-sans);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .metric[data-tone="idle"] .note {
    color: var(--text-soft);
  }
  /* A magnitude, never a verdict: the column's largest value fills the bar, and the track behind it shows the
     scale it fills, so a short bar reads as a share and not as a stray dash. */
  .bar {
    display: none;
    grid-area: bar;
    align-self: center;
    width: 100%;
    height: 4px;
    border-radius: 1px;
    background: linear-gradient(
        270deg,
        color-mix(in oklab, var(--tone) 62%, transparent)
          calc(var(--share) * 100%),
        var(--track) 0
      )
      no-repeat;
  }
  .value {
    grid-area: value;
    justify-self: end;
    font: 500 var(--type-md) / 1.35 var(--font-sans);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  /* A status word in a value's place is prose, not a figure. */
  .missing .value {
    color: var(--text-soft);
    font: var(--role-row);
  }
  /* The selected wash dims soft text below 4.5:1; muted keeps it, as on a running card. */
  .result-row[aria-current] time small,
  .result-row[aria-current] .missing .value,
  .result-row[aria-current] .metric[data-tone="idle"] .note {
    color: var(--text-muted);
  }
  .load-more {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    max-width: 1120px;
    padding: 0 var(--panel-pad) var(--space-5);
    color: var(--text-soft);
    font-size: var(--type-sm);
  }
  /* Where bars have the room, every row's value is as wide, so a column's bars share their zero end. */
  @container history-list (min-width: 901px) {
    .metric {
      grid-template-columns: minmax(0, 4.5rem) minmax(4.5rem, auto);
    }
    .bar {
      display: block;
    }
  }
  /* Compact: the time heads its row and the values share the width under it. */
  .history-list:global(.compact) .history-table {
    grid-template-columns: repeat(var(--metric-columns), minmax(0, 1fr));
    row-gap: var(--space-4);
  }
  .history-list:global(.compact) .column-head > button:first-child,
  .history-list:global(.compact) .outcome-head,
  .history-list:global(.compact) .head-icon {
    display: none;
  }
  .history-list:global(.compact) .column-head button {
    padding-inline: var(--space-2);
    font-size: var(--type-xs);
  }
  .history-list:global(.compact) .result-row {
    padding-block: 2px 4px;
  }
  .history-list:global(.compact) .result-row > * {
    padding: 4px var(--space-2);
  }
  /* The server beside its time, clear of an outcome at the row's end. */
  .history-list:global(.compact) .result-row > time {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    grid-column: 1 / -1;
    padding-inline-start: var(--space-3);
  }
  .history-list:global(.compact)
    .result-row:has(> .outcome:not(:empty))
    > time {
    padding-inline-end: 7rem;
  }
  .history-list:global(.compact) time small {
    flex: 1;
  }
  /* Five columns at the list's minimum leave a status like "Down only" two lines, not the neighbour's room. */
  .history-list:global(.compact) .missing .value {
    text-align: end;
    white-space: normal;
  }
  .history-list:global(.compact) .outcome {
    position: absolute;
    top: 4px;
    right: 0;
  }
  /* The detail replaces the list here, and the list's view control goes with it. */
  @container history (max-width: 820px) {
    .history-workspace:has(.has-detail) .history-head :global(.view-control) {
      display: none;
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
</style>
