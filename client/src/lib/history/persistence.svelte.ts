import type { store as applicationStore } from "../state/store.svelte";
import { HistoryWriteQueue } from "./writeQueue";
import { broadcastHistory, historyChanges, isHistoryChange } from "./changes";

/** Owns optional result persistence for one mounted application. */
export function mountHistoryPersistence(
  store: typeof applicationStore,
): () => void {
  let disposed = false;
  let historyRepository: import("./repository").HistoryRepository | null = null;
  let permanentHistoryWarning = false;
  const historyQueue = new HistoryWriteQueue(
    async (candidate, isCurrent, generation) => {
      const { HistoryRepository } = await import("./repository");
      if (disposed || !isCurrent()) return;
      historyRepository ??= new HistoryRepository();
      await historyRepository.put(candidate, generation);
    },
    async (id) => {
      await historyRepository?.delete(id);
    },
    (candidate) => {
      if (store.historyCandidate?.id === candidate.id)
        store.historyCandidate = null;
      if (!permanentHistoryWarning) store.historyWarning = "";
      broadcastHistory({ type: "put", id: candidate.id });
      window.dispatchEvent(new Event("graphite-meter-history-changed"));
    },
    (candidate) => {
      if (store.historyCandidate?.id === candidate.id)
        store.historyCandidate = null;
      permanentHistoryWarning = true;
      store.historyWarning =
        "This result could not be saved because it was malformed.";
    },
    () => {
      store.historyWarning =
        "Unable to save this result locally. Future writes will be retried.";
    },
  );
  const disposeEffects = $effect.root(() => {
    $effect(() => {
      const candidate = store.historyCandidate;
      if (!candidate) return;
      historyQueue.enqueue(candidate);
    });
  });
  const retry = () => void historyQueue.flush();
  const timer = window.setInterval(retry, 15_000);
  window.addEventListener("focus", retry);
  window.addEventListener("online", retry);
  document.addEventListener("visibilitychange", retry);
  const clearHistoryQueue = (generation: string) => {
    historyQueue.clear(generation);
    if (store.historyCandidate) store.historyCandidate = null;
    permanentHistoryWarning = false;
  };
  const onHistoryChange = (event: Event) => {
    const change = (event as CustomEvent).detail;
    if (isHistoryChange(change) && change.type === "clear")
      clearHistoryQueue(change.generation);
  };
  const stopHistoryChanges = historyChanges((change) => {
    if (change.type === "clear") clearHistoryQueue(change.generation);
  });
  window.addEventListener("graphite-meter-history-changed", onHistoryChange);
  return () => {
    disposed = true;
    disposeEffects();
    historyQueue.dispose();
    window.clearInterval(timer);
    window.removeEventListener("focus", retry);
    window.removeEventListener("online", retry);
    document.removeEventListener("visibilitychange", retry);
    window.removeEventListener(
      "graphite-meter-history-changed",
      onHistoryChange,
    );
    stopHistoryChanges();
    historyRepository?.close();
  };
}
