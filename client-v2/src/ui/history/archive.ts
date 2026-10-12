// The saved results one visit to History lists: read once as it opens, kept current by this tab's changes as they
// land, read again quietly on another tab's, and a linked result found even past the newest 2,000. Leaving stops a
// read still under way.
import { sortKeys, type HistoryRecord } from "../../app/history/records";
import type { HistoryChange, HistoryRepository } from "../../app/history/repository";
import { batch, computed, effect, onCleanup, signal, untrack, type Read } from "../../lib/signal";
import { applyChange, withRecord, type Listed } from "./model";

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
  /** What reading the linked result found when it is not in the list. */
  const answer = signal<{ id: string; status: "missing" | "malformed" } | null>(null);
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
    repository.list(controller.signal).then(
      ({ records, malformedCount }) => {
        if (reading !== controller) return;
        reading = null;
        let next: Listed = { records: sortKeys(records), malformed: malformedCount };
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
  const found = computed(() => {
    const id = selectedId();
    return id === null ? null : (records().find(({ record }) => record.id === id)?.record ?? null);
  });

  function lookUp(id: string): void {
    const at = changes;
    repository.inspect(id).then(
      (result) => {
        if (disposed || selectedId.peek() !== id || found.peek()) return;
        if (changes !== at) return lookUp(id);
        if (result.status === "ready")
          listed.set({ ...listed.peek(), records: withRecord(records.peek(), sortKeys([result.record])[0]!) });
        else answer.set({ id, status: result.status });
      },
      () => {
        if (!disposed && selectedId.peek() === id) error.set(UNREAD);
      },
    );
  }
  // A linked result outside the list is read on its own, once the list is.
  effect(() => {
    const id = selectedId();
    if (id === null || state() !== "ready" || found()) return;
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
    /** The linked result, or why there is none; pending while it is read, so nothing flashes "not found". */
    selected: computed<Selected>(() => {
      const id = selectedId();
      if (id === null) return NONE;
      const record = found();
      if (record) return { status: "ready", record };
      const known = answer();
      return state() === "ready" && known?.id === id ? { status: known.status } : PENDING;
    }),
    /** Reads the archive again; `quiet` keeps the list on screen meanwhile. */
    load,
  };
}
