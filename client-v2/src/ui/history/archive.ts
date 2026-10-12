// The saved results one visit to History lists, as entries small enough to keep 2,000: read once as it opens, kept
// current by this tab's changes as they land, and read again quietly on another tab's. The open result alone is read
// whole, even past the newest 2,000. Leaving stops a read still under way.
import { historyEntry, type HistoryRecord } from "../../app/history/records";
import type { HistoryChange, HistoryRepository } from "../../app/history/repository";
import { batch, computed, effect, onCleanup, signal, untrack, type Read } from "../../lib/signal";
import { applyChange, withEntry, type Listed } from "./model";

export type Selected =
  | { status: "none" | "pending" | "missing" | "malformed" }
  | { status: "ready"; record: HistoryRecord };

const NONE: Selected = { status: "none" };
const PENDING: Selected = { status: "pending" };
const STALE = "History could not be read again, so this list may be out of date.";
const UNREAD = "This result could not be read from History.";

/** Must run inside an owner: disposing it stops the read and the subscription. */
export function createArchive(repository: HistoryRepository, selectedId: Read<string | null>) {
  const state = signal<"loading" | "ready" | "error">("loading");
  const listed = signal<Listed>({ records: [], malformed: 0 });
  /** What reading the open result whole found: the one whole record History holds. */
  const answer = signal<{ id: string; selected: Selected } | null>(null);
  const error = signal("");
  let reading: AbortController | null = null;
  /** This tab's changes since the running read began: it may have read before some of them landed. */
  let missed: Exclude<HistoryChange, { type: "elsewhere" }>[] = [];
  /** Counts changes, so a lookup that raced one is made again. */
  let changes = 0;
  let disposed = false;

  function load(quiet = false): void {
    reading?.abort();
    const controller = new AbortController();
    reading = controller;
    missed = [];
    batch(() => {
      error.set("");
      if (!quiet) state.set("loading");
    });
    repository.list(controller.signal, historyEntry).then(
      ({ records, malformedCount }) => {
        if (reading !== controller) return;
        reading = null;
        let next: Listed = { records, malformed: malformedCount };
        for (const change of missed) next = applyChange(next, change);
        missed = [];
        batch(() => {
          listed.set(next);
          state.set("ready");
        });
      },
      () => {
        if (reading !== controller) return;
        reading = null;
        missed = [];
        // A list already shown stays, said to be stale.
        if (state.peek() === "ready") error.set(STALE);
        else state.set("error");
      },
    );
  }

  const stopChanges = repository.onChange((change) => {
    if (change.type === "elsewhere") return load(true);
    changes++;
    if (reading) missed.push(change);
    listed.set(applyChange(listed.peek(), change));
  });

  const records = computed(() => listed().records);
  const listedOpen = computed(() => {
    const id = selectedId();
    return id !== null && records().some((entry) => entry.id === id);
  });

  function lookUp(id: string): void {
    const at = changes;
    repository.inspect(id).then(
      (result) => {
        if (disposed || selectedId.peek() !== id) return;
        if (changes !== at) return lookUp(id);
        batch(() => {
          answer.set({ id, selected: result });
          // One past the newest 2,000 joins the list while it is open.
          if (result.status === "ready" && !listedOpen.peek())
            listed.set({ ...listed.peek(), records: withEntry(records.peek(), historyEntry(result.record)) });
        });
      },
      () => {
        if (!disposed && selectedId.peek() === id) error.set(UNREAD);
      },
    );
  }
  // The open result is read once the list is, and again as it leaves the list, so one deleted elsewhere reads as
  // missing; closing it lets go of it.
  effect(() => {
    const id = selectedId();
    const listedNow = listedOpen();
    if (id === null) return untrack(() => answer.set(null));
    if (state() !== "ready" || (listedNow && untrack(answer)?.id === id)) return;
    untrack(() => lookUp(id));
  });

  load();
  onCleanup(() => {
    disposed = true;
    reading?.abort();
    reading = null;
    stopChanges();
  });

  return {
    state: state as Read<"loading" | "ready" | "error">,
    records,
    malformed: computed(() => listed().malformed),
    /** Why this view's last change to, or read of, History failed; each list read clears it. */
    error,
    /** The open result, or why there is none; pending while it is read, so nothing flashes "not found". */
    selected: computed<Selected>(() => {
      const id = selectedId();
      if (id === null) return NONE;
      const known = answer();
      return known?.id === id ? known.selected : PENDING;
    }),
    /** Reads the archive again; `quiet` keeps the list on screen meanwhile. */
    load,
  };
}
