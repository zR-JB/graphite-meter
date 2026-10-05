import type { store as applicationStore } from "../state/store.svelte";
import type { HistoryRecord } from "./types";
import {
  announceHistoryChanged,
  HistoryRefusal,
  HistoryRepository,
} from "./repository";

/** Owns optional result persistence for one mounted application: saves in order, retrying transient failures. */
export function mountHistoryPersistence(
  store: typeof applicationStore,
): () => void {
  let disposed = false;
  let draining = false;
  const repository = new HistoryRepository();
  // Each result remembers the clear count it was queued under; an unread count is read before its write.
  const pending: { record: HistoryRecord; clears: Promise<number | null> }[] =
    [];
  // Records written or refused, so a result is never queued twice.
  const handled = new Set<string>();
  const settle = (record: HistoryRecord) => {
    pending.shift();
    handled.add(record.id);
    if (store.historyCandidate?.id === record.id) store.historyCandidate = null;
  };
  const drain = async () => {
    if (draining) return;
    draining = true;
    try {
      while (!disposed && pending.length) {
        const entry = pending[0];
        const { record } = entry;
        try {
          const clears = (await entry.clears) ?? (await repository.clears());
          entry.clears = Promise.resolve(clears);
          const written = await repository.put(record, clears);
          if (disposed) return;
          settle(record);
          store.historyWarning = "";
          if (written) announceHistoryChanged();
        } catch (error) {
          if (disposed) return;
          if (!(error instanceof HistoryRefusal)) {
            store.historyWarning =
              "Unable to save this result locally. Future writes will be retried.";
            return;
          }
          settle(record);
          store.historyWarning = error.message;
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
        clears: repository.clears().catch(() => null),
      });
      void drain();
    });
    // Saving switched on while a finished result is still on screen saves that result.
    $effect(() => {
      const latest = store.latestRecord;
      if (
        store.savingResults &&
        latest &&
        !handled.has(latest.id) &&
        !store.historyCandidate &&
        !pending.some((entry) => entry.record.id === latest.id)
      )
        store.historyCandidate = latest;
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
