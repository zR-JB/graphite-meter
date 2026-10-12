// The History workspace: saved results in a sortable list beside the one opened, the archive's notices, and the
// actions that change it. It lies over the console, which keeps measuring beneath.
import { HISTORY_SPLIT } from "../../app/preferences";
import "./history.css";
import type { App } from "../../app/app";
import type { HistorySort } from "../../app/history/records";
import type { HistoryRepository } from "../../app/history/repository";
import { epochMs } from "../../engine/run/clock";
import { Show } from "../../lib/dom";
import { batch, computed, effect, onCleanup, scope, signal, untrack, type Read } from "../../lib/signal";
import { announce } from "../kit/announce";
import { ConfirmDialog } from "../kit/dialog";
import { activeModal, canFocus, hasFocus } from "../kit/focus";
import { Icon } from "../kit/icon";
import { MoreMenu } from "../kit/menu";
import { Splitter } from "../kit/splitter";
import type { Shell } from "../shell/api";
import { createArchive, type Selected } from "./archive";
import { ResultDetail } from "./detail";
import { ResultsList, type Sort } from "./list";
import { span } from "./model";
import { ViewControl } from "./view";

const MINUTE_MS = 60_000;
const MIN_LIST_PX = 360;
const MIN_DETAIL_PX = 460;
/** From this width a result opens beside the list (history.css). */
const BESIDE_PX = 821;

/** Unreadable rows dismissed, per archive, for the page's life: the notice returns when their count changes. */
const dismissals = new WeakMap<HistoryRepository, number>();

const CONFIRM = {
  delete: {
    title: "Delete this result?",
    description: "This saved result is permanently removed from this browser.",
    cancel: "Keep result",
    confirm: "Delete result",
  },
  clear: {
    title: "Clear history?",
    description: "All saved results are permanently removed from this browser.",
    cancel: "Keep history",
    confirm: "Clear history",
  },
};
type Action = { kind: "delete"; id: string } | { kind: "clear" };

/** Built as History opens and disposed as it closes, which stops an archive read still under way. */
export function HistoryWorkspace(props: { app: App; shell: Shell; selectedId: Read<string | null> }): Node {
  const { app, shell } = props;
  const archive = createArchive(app.history, props.selectedId);
  const hasRecords = computed(() => archive.records().length > 0);
  let disposed = false;
  onCleanup(() => {
    disposed = true;
  });

  // Every visit opens on Date, newest first.
  const sort = signal<Sort>({ by: "date", descending: true });
  const pages = signal(1);
  let list: HTMLElement | null = null;
  const setSort = (by: HistorySort, descending: boolean) => {
    batch(() => {
      sort.set({ by, descending });
      pages.set(1);
    });
    list?.scrollTo({ top: 0 });
  };
  // A hidden column cannot order the list; Date takes over.
  effect(() => {
    const { by } = sort();
    if (by !== "date" && !app.preferences.historyColumns().includes(by)) untrack(() => setSort("date", true));
  });
  // How long ago a result finished reads in minutes while the page shows, and afresh as it shows again.
  const now = signal(epochMs());
  const tick = () => {
    if (document.visibilityState === "visible") now.set(epochMs());
  };
  const clock = setInterval(tick, MINUTE_MS);
  document.addEventListener("visibilitychange", tick);
  onCleanup(() => {
    clearInterval(clock);
    document.removeEventListener("visibilitychange", tick);
  });

  const actionError = archive.error;
  const dismissed = signal(dismissals.get(app.history) ?? 0);
  const unreadable = computed(() => (archive.malformed() !== dismissed() ? archive.malformed() : 0));
  const paused = computed(() => hasRecords() && !app.saving());
  const resume = () => app.preferences.set({ resultHistoryPreference: "enabled" });

  const asking = signal(false);
  const asked = signal<Action>({ kind: "clear" });
  let invoker: HTMLElement | null = null;
  const ask = (action: Action, from: HTMLElement) => {
    invoker = from;
    batch(() => {
      asked.set(action);
      asking.set(true);
    });
  };
  /** Focus that fell with what an action removed lands on Close History. */
  const refocus = () => {
    const close = workspace.querySelector<HTMLElement>(".close-history");
    if (!hasFocus() && !activeModal() && canFocus(close)) close.focus({ preventScroll: true });
  };

  async function act(): Promise<void> {
    const action = asked.peek();
    asking.set(false);
    actionError.set("");
    try {
      if (action.kind === "clear") await app.history.clear();
      else await app.history.delete(action.id);
    } catch {
      if (!disposed) actionError.set("History could not be changed. Try again.");
      return;
    }
    if (disposed) return;
    announce(action.kind === "clear" ? "History cleared." : "Result deleted.");
    const open = props.selectedId.peek();
    if (open !== null && (action.kind === "clear" || open === action.id)) shell.history(null);
    refocus();
  }

  async function removeUnreadable(): Promise<void> {
    actionError.set("");
    try {
      const removed = await app.history.removeMalformed();
      if (disposed) return;
      announce(`${removed} malformed ${removed === 1 ? "record" : "records"} removed.`);
      // None left to remove means another tab took them first: the count is stale.
      if (!removed) archive.load(true);
    } catch {
      if (!disposed) actionError.set("Unable to remove the malformed records.");
    }
    if (!disposed) workspace.focus({ preventScroll: true });
  }
  function dismiss(): void {
    dismissals.set(app.history, archive.malformed.peek());
    dismissed.set(archive.malformed.peek());
    workspace.focus({ preventScroll: true });
  }

  const Unavailable = (malformed: boolean) => (
    <div class="detail-pane empty-state" role="status">
      <span class="empty-icon">!</span>
      <h2>{malformed ? "Unreadable saved result" : "Result not found"}</h2>
      <p>
        {malformed ? "This record uses an unsupported format or failed validation." : "This result is not in History."}
      </p>
      <button class="btn" type="button" onclick={() => shell.history(null)}>
        Back to results
      </button>
    </div>
  );

  const Unread = () => (
    <div class="detail-pane empty-state" role="status">
      <span class="empty-icon">!</span>
      <h2>Result could not be read</h2>
      <p>History could not read this result just now.</p>
      <button class="btn" type="button" onclick={() => archive.reread()}>
        Retry
      </button>
      <button class="btn" type="button" onclick={() => shell.history(null)}>
        Back to results
      </button>
    </div>
  );

  // The result last focused, so a pane built again for it never takes focus twice.
  let focused: string | null = null;
  /** A result whose views threw: it reads as unreadable for the rest of the visit. */
  const broken = signal<string | null>(null);
  function DetailPane(selected: Selected): Node | null {
    if (selected.status === "none") focused = null;
    if (selected.status === "none" || selected.status === "pending") return null;
    if (selected.status === "unread") return Unread();
    if (selected.status !== "ready" || broken.peek() === selected.record.id)
      return Unavailable(selected.status !== "missing");
    const { record } = selected;
    let article: HTMLElement;
    try {
      article = (
        <ResultDetail
          app={app}
          record={record}
          onClose={() => shell.history(null)}
          onDelete={(from) => ask({ kind: "delete", id: record.id }, from)}
          onBroken={() => broken.set(record.id)}
        />
      ) as HTMLElement;
    } catch (error) {
      reportError(error);
      return Unavailable(true);
    }
    if (focused !== record.id) {
      focused = record.id;
      queueMicrotask(() => {
        if (!activeModal() && canFocus(article)) article.focus({ preventScroll: true });
      });
    }
    return (
      <div class="detail-pane enter" data-flip="history-detail">
        {article}
      </div>
    );
  }
  // Built again only when another result, or another state of the link, shows; a result still being read keeps what
  // shows, so moving between results never blanks the pane.
  let shownBefore = "none";
  const shownKey = computed(() => {
    const selected = archive.selected();
    if (selected.status === "pending") return shownBefore;
    shownBefore =
      selected.status !== "ready"
        ? selected.status
        : broken() === selected.record.id
          ? "malformed"
          : `ready\t${selected.record.id}`;
    return shownBefore;
  });

  function Body(): Node {
    const open = computed(() => props.selectedId() !== null);
    const width = signal(0);
    const listPx = computed(() =>
      Math.round(Math.max(MIN_LIST_PX, Math.min(width() - MIN_DETAIL_PX, app.preferences.historySplit() * width()))),
    );
    const results = ResultsList({
      app,
      shell,
      records: archive.records,
      sort,
      setSort,
      pages,
      selectedId: props.selectedId,
      now,
    });
    list = results.element;
    onCleanup(() => {
      list = null;
    });
    const body = (
      <div class="workspace-body">
        {list}
        <Splitter
          edge="right"
          label="Resize results list (arrow keys; Enter to reset)"
          value={listPx}
          min={computed(() => MIN_LIST_PX)}
          max={computed(() => width() - MIN_DETAIL_PX)}
          onChange={(px) => {
            if (width.peek() > 0) app.preferences.set({ historySplit: px / width.peek() });
          }}
          onReset={() => app.preferences.set({ historySplit: HISTORY_SPLIT })}
        />
      </div>
    ) as HTMLElement;
    // In this order, so the page lays out once: the pane that leaves goes before anything changes the list's width
    // (removing a focused result from a stale layout lays out every row just to move focus), then the list takes
    // its new width and fold together, then the pane that arrives.
    let pane: { key: string; node: Node | null; dispose: () => void } | null = null;
    effect(() => {
      const key = shownKey();
      const opened = open();
      untrack(() => {
        if (pane && pane.key !== key) {
          pane.dispose();
          pane.node?.parentNode?.removeChild(pane.node);
          pane = null;
        }
        body.classList.toggle("has-detail", opened);
        results.fitTo(opened && width.peek() >= BESIDE_PX ? listPx.peek() : width.peek());
        if (pane) return;
        const [node, dispose] = scope(() => DetailPane(archive.selected()));
        if (node) body.append(node);
        pane = { key, node, dispose };
      });
    });
    onCleanup(() => pane?.dispose());
    // The list keeps its share of the width as the window changes; CSS applies it, the handle reports it.
    effect(() => body.style.setProperty("--split", String(app.preferences.historySplit())));
    const resized = new ResizeObserver(() => width.set(body.clientWidth));
    resized.observe(body);
    onCleanup(() => resized.disconnect());
    return body;
  }

  const view = computed(() => {
    const state = archive.state();
    return state !== "ready" ? state : hasRecords() ? "records" : "empty";
  });
  const content = (shown: ReturnType<typeof view>): Node => {
    if (shown === "records") return <Body />;
    if (shown === "loading")
      return (
        <div class="empty-state" role="status">
          <span class="empty-icon">
            <Icon name="history" />
          </span>
          <h2>Opening History</h2>
        </div>
      );
    if (shown === "error")
      return (
        <div class="empty-state" data-tone="err" role="alert">
          <span class="empty-icon">!</span>
          <h2>History is unavailable</h2>
          <p>The browser could not open its saved results.</p>
          <button class="btn btn-accent" type="button" onclick={() => archive.load()}>
            Retry
          </button>
        </div>
      );
    return (
      <div class="empty-state">
        <span class="empty-icon">
          <Icon name="history" />
        </span>
        <h2>No saved results</h2>
        {() =>
          app.saving() ? (
            <p>Completed tests appear here automatically.</p>
          ) : (
            [
              <p>Saving is paused. Resume it to keep future results in this browser.</p>,
              <button class="btn btn-accent" type="button" onclick={resume}>
                Resume saving
              </button>,
            ]
          )
        }
      </div>
    );
  };

  const workspace = (
    <section class="history-workspace enter" aria-labelledby="history-title" tabindex="-1">
      <header class="sheet-head history-head">
        <h1 id="history-title">History</h1>
        <Show when={hasRecords}>
          {() => (
            <dl class="head-facts">
              <div>
                <dt>Results</dt>
                <dd>{() => String(archive.records().length)}</dd>
              </div>
              <div>
                <dt>Span</dt>
                <dd>{() => span(archive.records())}</dd>
              </div>
            </dl>
          )}
        </Show>
        <div class="head-actions">
          <Show when={hasRecords}>
            {() => (
              <ViewControl
                columns={app.preferences.historyColumns}
                onColumns={(historyColumns) => app.preferences.set({ historyColumns })}
                sort={sort}
                onSort={setSort}
              />
            )}
          </Show>
          <Show when={() => hasRecords() || archive.malformed() > 0}>
            {() => (
              <MoreMenu
                label="History actions"
                danger
                items={[{ title: "Clear history", icon: "trash", run: (trigger) => ask({ kind: "clear" }, trigger) }]}
              />
            )}
          </Show>
          <button
            class="btn btn-icon btn-quiet close-history"
            type="button"
            aria-label="Close History"
            onclick={() => shell.closeHistory()}
          >
            <Icon name="close" />
          </button>
        </div>
      </header>
      <Show when={() => paused() || !!app.saver.warning() || !!actionError() || unreadable() > 0}>
        {() => (
          <div class="notices">
            <Show when={paused}>
              {() => (
                <p class="notice" data-tone="warn">
                  <span>
                    <strong>Saving is paused.</strong> Saved results remain.
                  </span>
                  <button class="btn" type="button" onclick={resume}>
                    Resume saving
                  </button>
                </p>
              )}
            </Show>
            <Show when={app.saver.warning}>
              {(text) => (
                <p class="notice" data-tone="warn" role="status">
                  {text}
                </p>
              )}
            </Show>
            <Show when={actionError}>
              {(text) => (
                <p class="notice" data-tone="warn" role="status">
                  {text}
                </p>
              )}
            </Show>
            <Show when={unreadable}>
              {(count) => (
                <p class="notice" data-tone="warn" role="status">
                  <span>
                    {() =>
                      count() === 1
                        ? "1 unsupported or malformed record was ignored."
                        : `${count()} unsupported or malformed records were ignored.`
                    }
                  </span>
                  <span class="notice-actions">
                    <button class="btn" type="button" onclick={() => void removeUnreadable()}>
                      Remove them
                    </button>
                    <button class="btn btn-quiet" type="button" onclick={dismiss}>
                      Dismiss
                    </button>
                  </span>
                </p>
              )}
            </Show>
          </div>
        )}
      </Show>
      {() => {
        const shown = view();
        return untrack(() => content(shown));
      }}
      <ConfirmDialog
        open={asking}
        id="history-confirm"
        title={() => CONFIRM[asked().kind].title}
        description={() => CONFIRM[asked().kind].description}
        cancel={() => CONFIRM[asked().kind].cancel}
        confirm={() => CONFIRM[asked().kind].confirm}
        invoker={() => invoker}
        onCancel={() => asking.set(false)}
        onConfirm={() => void act()}
      />
    </section>
  ) as HTMLElement;
  return workspace;
}
