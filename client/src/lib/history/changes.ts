export type HistoryChange =
  | { type: "put" | "delete"; id?: string }
  | { type: "clear"; generation: string };

const GENERATION_KEY = "graphite-meter:history-generation";
const MAX_GENERATION_LENGTH = 128;
const GENERATION = /^[A-Za-z0-9._-]+$/;
const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export function isHistoryGeneration(value: unknown): value is string {
  return (
    value === "" ||
    (typeof value === "string" &&
      value.length <= MAX_GENERATION_LENGTH &&
      GENERATION.test(value))
  );
}

export function isHistoryChange(value: unknown): value is HistoryChange {
  if (value === null || typeof value !== "object" || Array.isArray(value))
    return false;
  const change = value as Record<string, unknown>;
  if (change.type === "clear") {
    if (
      !Object.keys(change).every(
        (key) => key === "type" || key === "generation",
      )
    )
      return false;
    return (
      typeof change.generation === "string" &&
      change.generation !== "" &&
      isHistoryGeneration(change.generation)
    );
  }
  if (change.type !== "put" && change.type !== "delete") return false;
  if (!Object.keys(change).every((key) => key === "type" || key === "id"))
    return false;
  return (
    change.id === undefined ||
    (typeof change.id === "string" && UUID.test(change.id))
  );
}

export function currentHistoryGeneration(): string {
  try {
    const generation = localStorage.getItem(GENERATION_KEY) ?? "";
    return isHistoryGeneration(generation) ? generation : "";
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
  channel.onmessage = (event) => {
    if (isHistoryChange(event.data)) onChange(event.data);
  };
  return () => channel.close();
}

export function broadcastHistory(change: HistoryChange): void {
  if (typeof BroadcastChannel === "undefined" || !isHistoryChange(change))
    return;
  const channel = new BroadcastChannel("graphite-meter-history");
  channel.postMessage(change);
  channel.close();
}
