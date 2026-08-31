export type HistoryChange =
  | { type: "put" | "delete"; id?: string }
  | { type: "clear"; generation: string };

const GENERATION_KEY = "graphite-meter:history-generation";

export function currentHistoryGeneration(): string {
  try {
    return localStorage.getItem(GENERATION_KEY) ?? "";
  } catch {
    return "";
  }
}

export function nextHistoryGeneration(): string {
  const generation =
    typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now()}-${Math.random()}`;
  try {
    localStorage.setItem(GENERATION_KEY, generation);
  } catch {
    // BroadcastChannel and the local event still protect tabs when storage is unavailable.
  }
  return generation;
}

export function restoreHistoryGeneration(generation: string): void {
  try {
    if (generation) localStorage.setItem(GENERATION_KEY, generation);
    else localStorage.removeItem(GENERATION_KEY);
  } catch {
    // The durable IndexedDB generation remains authoritative when storage is unavailable.
  }
}

export function historyChanges(
  onChange: (change: HistoryChange) => void,
): () => void {
  if (typeof BroadcastChannel === "undefined") return () => {};
  const channel = new BroadcastChannel("graphite-meter-history");
  channel.onmessage = (event) => onChange(event.data as HistoryChange);
  return () => channel.close();
}

export function broadcastHistory(change: HistoryChange): void {
  if (typeof BroadcastChannel === "undefined") return;
  const channel = new BroadcastChannel("graphite-meter-history");
  channel.postMessage(change);
  channel.close();
}
