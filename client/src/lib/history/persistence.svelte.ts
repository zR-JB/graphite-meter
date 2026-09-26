import type { store as applicationStore } from "../state/store.svelte";
import type { HistoryRecord } from "./types";
import { announceHistoryChanged, HistoryRepository } from "./repository";

/** Owns optional result persistence for one mounted application: saves in order, retrying transient failures. */
export function mountHistoryPersistence(
  store: typeof applicationStore,
): () => void {
  let disposed = false;
  let draining = false;
  const repository = new HistoryRepository();
  // Each result remembers the clear count it was queued under; unreadable storage counts as none.
  const pending: { record: HistoryRecord; clears: Promise<number> }[] = [];
  const settle = (record: HistoryRecord) => {
    pending.shift();
    if (store.historyCandidate?.id === record.id) store.historyCandidate = null;
  };
  const drain = async () => {
    if (draining) return;
    draining = true;
    try {
      while (!disposed && pending.length) {
        const { record, clears } = pending[0];
        try {
          const written = await repository.put(record, await clears);
          if (disposed) return;
          settle(record);
          store.historyWarning = "";
          if (written) announceHistoryChanged();
        } catch (error) {
          if (disposed) return;
          // A value storage cannot clone never succeeds on retry.
          if (!(
            error instanceof DOMException && error.name === "DataCloneError"
          )) {
            store.historyWarning =
              "Unable to save this result locally. Future writes will be retried.";
            return;
          }
          settle(record);
          store.historyWarning =
            "This result could not be saved in browser storage.";
        }
      }
    } finally {
      draining = false;
    }
  };
  const disposeEffects = $effect.root(() => {
    $effect(() => {
      const candidate = store.historyCandidate;
      if (
        !candidate ||
        pending.some((entry) => entry.record.id === candidate.id)
      )
        return;
      pending.push({
        record: candidate,
        clears: repository.clears().catch(() => 0),
      });
      void drain();
    });
  });
  const retry = () => void drain();
  const timer = window.setInterval(retry, 15_000);
  window.addEventListener("focus", retry);
  window.addEventListener("online", retry);
  document.addEventListener("visibilitychange", retry);
  return () => {
    disposed = true;
    disposeEffects();
    window.clearInterval(timer);
    window.removeEventListener("focus", retry);
    window.removeEventListener("online", retry);
    document.removeEventListener("visibilitychange", retry);
    repository.close();
  };
}
