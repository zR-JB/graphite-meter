// The results list: sortable heads, rows grouped by day, pages of 50 added on demand, each row built once.
import type { App } from "../../app/app";
import {
  naturalDescending,
  recentCompletion,
  sortHistory,
  type HistorySort,
  type HistoryEntry,
} from "../../app/history/records";
import { For, Show } from "../../lib/dom";
import { animate, nextFrame } from "../../lib/frame";
import { insert } from "../../lib/jsx-runtime";
import { reducedMotion } from "../../lib/motion";
import {
  batch,
  computed,
  effect,
  onCleanup,
  signal,
  untrack,
  type Read,
  type Signal,
} from "../../lib/signal";
import { OUTCOME } from "../../view/words";
import { activeModal, canFocus } from "../kit/focus";
import { Icon } from "../kit/icon";
import { tip } from "../kit/tip";
import type { Shell } from "../shell/api";
import {
  COLUMN,
  columnScales,
  daysAt,
  groupRows,
  orderHeading,
  PAGE_ROWS,
  rowPresenter,
  SORT_LABEL,
  type Cell,
  type Days,
  type Row,
} from "./model";

export interface Sort {
  by: HistorySort;
  descending: boolean;
}

const HOUR_MS = 3_600_000;
/** From this list width the bars show (`data-bars`), and the content needs a different width. */
const BARS_PX = 901;
/** One side of BARS_PX: the narrowest list the unfolded content was seen to fit, and the width it was measured to
    need, exact until rows are added. */
const unknown = () => ({ fits: Infinity, needs: 0, exact: false });

/** Rows built in one frame: more than a tall screen shows. */
const BUILD_ROWS = 16;
export function ResultsList(props: {
  app: App;
  shell: Shell;
  records: Read<readonly HistoryEntry[]>;
  sort: Read<Sort>;
  setSort(by: HistorySort, descending: boolean): void;
  pages: Signal<number>;
  selectedId: Read<string | null>;
  /** Epoch ms, a minute apart. */
  now: Read<number>;
}): {
  element: HTMLElement;
  fitTo(width: number): void;
  neighbor(id: string, by: 1 | -1): string | null;
} {
  const { app } = props;
  const columns = app.preferences.historyColumns;
  const units = computed(() => ({
    base: app.preferences.unitBase(),
    kind: app.preferences.unitKind(),
  }));
  const scales = computed(() => columnScales(props.records(), units()));
  // The same object all day, so rows are dated again only when it turns.
  let day: Days | undefined;
  const days = computed(() => {
    const next = daysAt(props.now());
    return day?.today === next.today && day.yesterday === next.yesterday ? day : (day = next);
  });
  const byDay = computed(() => props.sort().by === "date");
  // Rows are worded again only when something they show changes; a peak only rescales the bars.
  const wording = computed(() =>
    [
      columns().join(),
      units().base,
      units().kind,
      byDay(),
      ...columns().map((column) => scales()[column].tier),
    ].join(),
  );
  const present = computed(() => {
    wording();
    return untrack(() =>
      rowPresenter({ columns: columns(), scales: scales(), units: units(), byDay: byDay() }),
    );
  });
  const ordered = computed(() =>
    sortHistory(props.records(), props.sort().by, props.sort().descending),
  );
  // Only the pages asked for: a link to a result far down opens it without building every row above it.
  const shown = computed(() => Math.min(ordered().length, props.pages() * PAGE_ROWS));
  // A screenful first, or every row down to the open result's when it lies further; the rest of the pages a screenful
  // a frame after, below the fold.
  const built = signal(BUILD_ROWS);
  const selectedAt = computed(() => {
    const id = props.selectedId();
    return id === null ? -1 : ordered().findIndex((entry) => entry.id === id);
  });
  // The result last closed: its row is built and shown, even past the pages asked for, so closing returns to it.
  const returned = signal<string | null>(null);
  const returnedAt = computed(() => {
    const id = returned();
    return id === null ? -1 : ordered().findIndex((entry) => entry.id === id);
  });
  const limit = computed(() =>
    Math.min(shown(), Math.max(built(), selectedAt() + 1, returnedAt() + 1)),
  );
  effect(() => {
    shown();
    untrack(() => {
      if (limit.peek() >= shown.peek()) return;
      onCleanup(
        animate(() => {
          built.set(built.peek() + BUILD_ROWS);
          return limit.peek() < shown.peek();
        }),
      );
    });
  });
  const visible = computed(() => {
    const [row, today] = [present(), days()];
    return ordered()
      .slice(0, limit())
      .map((entry) => row(entry, today));
  });
  const groups = computed(() => groupRows(visible()));
  const ordering = computed(() => orderHeading(props.sort().by, props.sort().descending));

  // The open result's row, marked by one write per change of selection rather than a binding per row.
  const links = new Map<string, HTMLAnchorElement>();
  let current: HTMLAnchorElement | undefined;
  const markCurrent = (link: HTMLAnchorElement | undefined) => {
    current?.removeAttribute("aria-current");
    current = link;
    current?.setAttribute("aria-current", "true");
  };
  // A result opened, or stepped to, brings its row into the list's view below the sticky heads, once its row is built.
  let revealing: (() => void) | null = null;
  const reveal = () => {
    revealing = null;
    if (!current) return;
    const view = list.getBoundingClientRect();
    const row = current.getBoundingClientRect();
    const top = view.top + (table.querySelector<HTMLElement>(".column-head")?.offsetHeight ?? 0);
    const by =
      row.top < top ? row.top - top : row.bottom > view.bottom ? row.bottom - view.bottom : 0;
    if (by) list.scrollBy({ top: by, behavior: reducedMotion.peek() ? "instant" : "smooth" });
  };
  effect(() => {
    const id = props.selectedId();
    untrack(() => markCurrent(id === null ? undefined : links.get(id)));
    if (id !== null) revealing ??= nextFrame(reveal);
  });
  onCleanup(() => revealing?.());
  // Closing a result returns focus to its row, or to Close History once the row is gone; after the panes settle.
  let open = props.selectedId.peek();
  effect(() => {
    const id = props.selectedId();
    const closed = open;
    open = id;
    if (id !== null || closed === null) return;
    untrack(() => {
      const at = ordered().findIndex((entry) => entry.id === closed);
      if (at < 0) return;
      batch(() => {
        props.pages.set(Math.max(props.pages(), Math.ceil((at + 1) / PAGE_ROWS)));
        returned.set(closed);
      });
    });
    queueMicrotask(() => {
      if (activeModal() || props.selectedId.peek() !== null) return;
      const target =
        links.get(closed) ??
        list.closest(".history-workspace")?.querySelector<HTMLElement>(".close-history");
      if (canFocus(target)) target.focus();
    });
  });

  /** Whether each row's server line is cut, found as a hand or focus reaches it: no observer per row. */
  const cut = new WeakMap<Element, Signal<boolean>>();
  const checkCut = (event: Event) => {
    const line = (event.target as Element).closest?.(".result-row")?.querySelector("time small");
    if (line) cut.get(line)?.set(line.scrollWidth > line.clientWidth + 1);
  };

  const Metric = (cell: Cell) => (
    <span class={cell.value === null ? "metric missing" : "metric"} data-tone={cell.column}>
      {cell.value !== null && (
        <span class="bar" ref={(bar) => bar.style.setProperty("--value", String(cell.value))} />
      )}
      <span class="value">{cell.text}</span>
      {cell.note && <small class="note">{cell.note}</small>}
    </span>
  );

  function RowView({ row }: { row: Row }): Node {
    const href = `#/history/${encodeURIComponent(row.id)}`;
    let link: HTMLAnchorElement;
    if (row.kind === "unreadable")
      link = (
        <a class="result-row link-row" data-history-id={row.id} href={href}>
          <time datetime={row.iso}>{row.date}</time>
          <span class="outcome">
            <span class="status-dot inline" data-tone="warn" />
            Unreadable
          </span>
        </a>
      ) as HTMLAnchorElement;
    else {
      // Only a result from the last hour says how long ago; the rest never follow the clock.
      const fresh = props.now.peek() - row.completedAt < HOUR_MS;
      const line = fresh
        ? computed(() =>
            [row.where, recentCompletion(row.completedAt, props.now())].filter(Boolean).join(", "),
          )
        : () => row.where;
      const clipped = signal(false);
      const server = (fresh || row.where) && (
        <small ref={tip(() => (clipped() ? line() : ""), "clip")}>{fresh ? line : row.where}</small>
      );
      if (server) cut.set(server as Element, clipped);
      const outcome = OUTCOME[row.outcome];
      link = (
        <a class="result-row link-row" data-history-id={row.id} href={href} aria-label={row.label}>
          <time datetime={row.iso} title={row.exact}>
            {row.primary}
            {server}
          </time>
          <span class="outcome">
            {row.outcome !== "complete" && [
              <span class="status-dot inline" data-tone={outcome.tone} />,
              outcome.label,
            ]}
          </span>
          {row.cells.map(Metric)}
        </a>
      ) as HTMLAnchorElement;
    }
    links.set(row.id, link);
    if (props.selectedId.peek() === row.id) markCurrent(link);
    onCleanup(() => {
      if (links.get(row.id) === link) links.delete(row.id);
      if (current === link) current = undefined;
    });
    return <li>{link}</li>;
  }

  function Head(column: HistorySort): Node {
    const sorted = computed(() => props.sort().by === column);
    const order = () => (props.sort().descending ? "descending" : "ascending");
    const unit = () => (column === "date" ? "" : scales()[column].unit);
    const name = () =>
      column === "date" ? SORT_LABEL.date : `${COLUMN[column].short} ${unit()}`.trim();
    return (
      <button
        type="button"
        aria-pressed={() => String(sorted())}
        data-order={() => (sorted() ? order() : undefined)}
        aria-label={() => (sorted() ? `${name()}, ${order()}` : undefined)}
        onclick={() =>
          props.setSort(
            column,
            sorted.peek() ? !props.sort.peek().descending : naturalDescending(column),
          )
        }
      >
        {column === "date" ? (
          <span>{SORT_LABEL.date}</span>
        ) : (
          [
            <span class="head-icon" data-tone={column}>
              <Icon name={COLUMN[column].icon} />
            </span>,
            // The unit belongs to the explained word, so a tip that flips below never covers it.
            <span ref={tip(() => COLUMN[column].tip)}>
              {COLUMN[column].short}
              {() => unit() && <span class="unit">{unit()}</span>}
            </span>,
          ]
        )}
        <i aria-hidden="true" />
      </button>
    );
  }

  const table = (
    <div
      class="history-table"
      data-sort={() => props.sort().by}
      onclick={(event) => {
        const row = (event.target as Element).closest<HTMLAnchorElement>("a.result-row");
        // A modified or secondary click follows the link: a new tab or window.
        if (
          !row ||
          event.button !== 0 ||
          event.metaKey ||
          event.ctrlKey ||
          event.shiftKey ||
          event.altKey
        )
          return;
        event.preventDefault();
        const id = row.dataset.historyId!;
        props.shell.history(props.selectedId.peek() === id ? null : id);
      }}
      onpointerover={checkCut}
      onfocusin={checkCut}
    >
      <div class="column-head page-fill" role="group" aria-label="Sort by">
        {Head("date")}
        <span class="outcome-head" />
        <For each={columns} by={(column) => column}>
          {(column) => Head(column.peek())}
        </For>
      </div>
      <For each={groups} by={(group) => group.key}>
        {(group, index) => {
          const { heading } = group.peek();
          const id = () => `history-day-${index()}`;
          return (
            <section class="group day">
              {heading ? (
                <h3 id={id}>{heading}</h3>
              ) : (
                <div class="group-head">
                  <h3 id={id}>{ordering}</h3>
                  <button
                    class="btn btn-quiet by-date"
                    type="button"
                    onclick={() => props.setSort("date", true)}
                  >
                    Sort by date
                  </button>
                </div>
              )}
              <ol aria-labelledby={id}>
                <For each={() => group().rows} by={(row) => row.id}>
                  {(row) => () => <RowView row={row()} />}
                </For>
              </ol>
            </section>
          );
        }}
      </For>
    </div>
  ) as HTMLElement;
  effect(() => table.style.setProperty("--metric-columns", String(columns().length)));
  // Each column's bars fill against its peak, so a new fastest result redraws them without rebuilding a row; a peak
  // that stays is not written again, which would restyle every row.
  const peaks = new Map<string, string>();
  effect(() => {
    for (const column of columns()) {
      const peak = String(scales()[column].peak || 1);
      if (peaks.get(column) === peak) continue;
      peaks.set(column, peak);
      table.style.setProperty(`--peak-${column}`, peak);
    }
  });

  const list = (<div class="history-list" data-flip="history-list" />) as HTMLElement;
  const loadMore = () => props.pages.set(props.pages.peek() + 1);
  insert(list, [
    table,
    <Show when={() => shown() < ordered().length}>
      {() => {
        const footer = (
          <div class="load-more">
            <button class="btn" type="button" onclick={loadMore}>
              {`Load ${PAGE_ROWS} more`}
            </button>
            <span>{() => `${shown()} of ${ordered().length}`}</span>
          </div>
        ) as HTMLElement;
        // Within the list's own scroller, so the margin loads the next page before the footer shows.
        const near = new IntersectionObserver(
          (entries) => {
            if (entries.some((entry) => entry.isIntersecting)) loadMore();
          },
          { root: list, rootMargin: "200px 0px" },
        );
        near.observe(footer);
        onCleanup(() => near.disconnect());
        return footer;
      }}
    </Show>,
  ]);

  // Rows fold once the table's columns no longer fit their content, judged on width alone. The content's own width
  // differs on either side of BARS_PX, so each side keeps what it learned until the rows change.
  let folded = false;
  let sides = [unknown(), unknown()];
  /** The list's scrollbar gutter, which its expected width includes. */
  let gutter = 0;
  const fold = (on: boolean) => {
    folded = on;
    list.classList.toggle("compact", on);
  };
  const bars = (width: number) => list.toggleAttribute("data-bars", width >= BARS_PX);
  /** Whether the unfolded content fits a list this wide, or null when only measuring it can tell. */
  const judge = (width: number): boolean | null => {
    const side = sides[width >= BARS_PX ? 1 : 0]!;
    if (width >= side.fits) return true;
    if (width + 1 < side.needs) return false;
    return side.exact ? true : null;
  };
  // Unfolded, the content's overflow says which way it fits; in a ResizeObserver callback that read costs nothing.
  const learn = (width: number) => {
    const side = sides[width >= BARS_PX ? 1 : 0]!;
    const needs = list.scrollWidth;
    if (needs <= width + 1) side.fits = Math.min(side.fits, width);
    else Object.assign(side, { needs, exact: true });
    fold(needs > width + 1);
  };
  const fit = () => {
    const width = list.clientWidth;
    gutter = list.offsetWidth - width;
    bars(width);
    const fits = folded ? judge(width) : null;
    if (fits !== null) return fold(!fits);
    // Unfolded, or folded with nothing known for this width: measure, unfolding first (one more layout).
    fold(false);
    learn(width);
  };
  let scheduled: (() => void) | null = null;
  const refit = () => {
    sides = [unknown(), unknown()];
    scheduled ??= nextFrame(() => {
      scheduled = null;
      // A resize may have measured it since.
      if (judge(list.clientWidth) === null) {
        fold(false);
        learn(list.clientWidth);
      }
    });
  };
  const resized = new ResizeObserver(fit);
  resized.observe(list);
  resized.observe(table);
  let disposed = false;
  void document.fonts.ready.then(() => !disposed && refit());
  effect(() => {
    present();
    ordered();
    untrack(refit);
  });
  // Added rows can only widen the content: what it needed stays a floor, where it fitted is no longer known.
  effect(() => {
    shown();
    for (const side of sides) Object.assign(side, { fits: Infinity, exact: false });
  });
  onCleanup(() => {
    disposed = true;
    resized.disconnect();
    scheduled?.();
  });

  return {
    element: list,
    /** The list is about to be `width` px wide (a result opening or closing): a fold decided now lands in the layout
        that width takes rather than one more. */
    fitTo(width) {
      bars(width - gutter);
      const fits = judge(width - gutter);
      if (fits !== null) fold(!fits);
    },
    /** The result `by` rows from `id` in the list's order, its page added to those shown; null past either end. */
    neighbor(id, by) {
      const order = ordered.peek();
      const at = order.findIndex((entry) => entry.id === id);
      const next = at < 0 ? undefined : order[at + by];
      if (!next) return null;
      props.pages.set(Math.max(props.pages.peek(), Math.ceil((at + by + 1) / PAGE_ROWS)));
      return next.id;
    },
  };
}
