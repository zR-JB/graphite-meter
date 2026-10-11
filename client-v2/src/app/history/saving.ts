// Saving finished results to History, one write at a time and in order. A write carries the clear count read when it
// was queued and is dropped when History was cleared since, in any tab; a refusal is final, anything else retried.
import { batch, effect, signal, type Read } from "../../lib/signal";
import type { HistoryRecord } from "./records";
import { HistoryRefusal, type HistoryRepository } from "./repository";

const RETRY_MS = 15_000;
const RETRYING = "Unable to save this result locally. Future writes will be retried.";

export interface HistorySaving {
  /** The latest finished result: saved now while saving is on, else the moment it turns on in any tab, until
      withdrawn. */
  offer(record: HistoryRecord): void;
  /** A new Start or Start over: the result shown before can no longer be saved, though queued writes go on. */
  withdraw(): void;
  /** Writes are queued or retrying; signing out waits for them. */
  readonly pending: Read<boolean>;
  /** The offered result is not in History: saving is off, its write is queued or failed, or it is gone since. */
  readonly unsaved: Read<boolean>;
  /** Why the last write failed, until one succeeds. */
  readonly warning: Read<string>;
  dispose(): void;
}

interface Entry {
  record: HistoryRecord;
  clears: Promise<number | null>;
}

export function createHistorySaving(
  repository: Pick<HistoryRepository, "clears" | "put" | "inspect" | "onChange">,
  saving: Read<boolean>,
  target: Pick<Window, "addEventListener" | "removeEventListener">,
): HistorySaving {
  const pending = signal(false);
  const unsaved = signal(false);
  const warning = signal("");
  const queue: Entry[] = [];
  // Every record written, refused or dropped, never queued again: true while it is in History.
  const settled = new Map<string, boolean>();
  let offered: HistoryRecord | null = null;
  let draining = false;
  let disposed = false;
  let stopRetries: (() => void) | null = null;

  const showUnsaved = () => unsaved.set(offered !== null && settled.get(offered.id) !== true);

  const enqueue = (record: HistoryRecord) => {
    if (settled.has(record.id) || queue.some((entry) => entry.record.id === record.id)) return;
    queue.push({ record, clears: repository.clears().catch(() => null) });
    pending.set(true);
    void drain();
  };

  const settle = (entry: Entry, written: boolean, message: string) =>
    batch(() => {
      queue.shift();
      settled.set(entry.record.id, written);
      pending.set(queue.length > 0);
      warning.set(message);
      showUnsaved();
    });

  const drain = async () => {
    if (draining) return;
    draining = true;
    try {
      while (!disposed && queue.length) {
        const entry = queue[0]!;
        try {
          const clears = (await entry.clears) ?? (await repository.clears());
          entry.clears = Promise.resolve(clears);
          const written = await repository.put(entry.record, clears);
          if (disposed) return;
          settle(entry, written, "");
        } catch (error) {
          if (disposed) return;
          if (!(error instanceof HistoryRefusal)) {
            warning.set(RETRYING);
            return retryUntilSaved();
          }
          settle(entry, false, error.message);
        }
      }
      stopRetries?.();
    } finally {
      draining = false;
    }
  };

  const retry = () => void drain();
  const retryUntilSaved = () => {
    if (stopRetries) return;
    const timer = setInterval(retry, RETRY_MS);
    // visibilitychange bubbles from the document to the window.
    const events = ["focus", "online", "visibilitychange"];
    for (const type of events) target.addEventListener(type, retry);
    stopRetries = () => {
      clearInterval(timer);
      for (const type of events) target.removeEventListener(type, retry);
      stopRetries = null;
    };
  };

  const removed = (id: string) => {
    if (!settled.get(id)) return;
    settled.set(id, false);
    showUnsaved();
  };
  const stopWatching = repository.onChange((change) => {
    if (change.type === "deleted") for (const id of change.ids) removed(id);
    else if (change.type === "cleared") for (const id of settled.keys()) removed(id);
    else if (change.type === "elsewhere" && offered && settled.get(offered.id)) {
      const { id } = offered;
      repository.inspect(id).then(
        ({ status }) => {
          if (status !== "ready") removed(id);
        },
        () => {},
      );
    }
  });

  // Saving switched on, here or in another tab, saves the result still shown.
  const stopFollowing = effect(() => {
    if (saving() && offered) enqueue(offered);
  });

  return {
    offer(record) {
      offered = record;
      batch(() => {
        if (saving.peek()) enqueue(record);
        showUnsaved();
      });
    },
    withdraw() {
      offered = null;
      showUnsaved();
    },
    pending,
    unsaved,
    warning,
    dispose() {
      disposed = true;
      stopFollowing();
      stopWatching();
      stopRetries?.();
    },
  };
}
