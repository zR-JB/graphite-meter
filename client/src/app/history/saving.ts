// Saving finished results to History, one write at a time and in order. A write carries the clear count read when it
// was queued and is dropped when History was cleared since, in any tab; a refusal is final, anything else retried.
import { batch, effect, signal, type Read } from "../../lib/signal";
import type { HistoryRecord } from "./records";
import { HistoryRefusal, type HistoryRepository } from "./repository";

const RETRY_MS = 15_000;
/** Results waiting while storage fails; past this the oldest waiting one is given up, so memory stays bounded. */
const MAX_QUEUED = 8;
const RETRYING = "Could not save this result locally. Future writes will be retried.";
const UNORDERED = "Could not save this result: History could not be read when it finished.";

interface HistorySaving {
  /** The latest finished result: saved now while saving is on, else the moment it turns on in any tab, until
      withdrawn. */
  offer(record: HistoryRecord): void;
  /** A new Start or Start over: the result shown before can no longer be saved, though queued writes go on. */
  withdraw(): void;
  /** Writes are queued or retrying; signing out waits for them. */
  readonly pending: Read<boolean>;
  /** The offered result is not in History: saving is off, its write is queued or failed, or it is gone since. */
  readonly unsaved: Read<boolean>;
  /** The offered result's write was refused or given up: not saved, though saving is on and nothing removed it. */
  readonly failed: Read<boolean>;
  /** Why the last write failed, until one succeeds. */
  readonly warning: Read<string>;
  dispose(): void;
}

interface Entry {
  record: HistoryRecord;
  clears: Promise<number | null | HistoryRefusal>;
  /** `changes` when it was queued. */
  queuedAt: number;
}

export function createHistorySaving(
  repository: Pick<HistoryRepository, "clears" | "put" | "inspect" | "onChange">,
  saving: Read<boolean>,
  target: Pick<Window, "addEventListener" | "removeEventListener">,
): HistorySaving {
  const pending = signal(false);
  const unsaved = signal(false);
  const failed = signal(false);
  const warning = signal("");
  const queue: Entry[] = [];
  // Every record written, refused or dropped, never queued again: true while it is in History.
  const settled = new Map<string, boolean>();
  // Records whose write was refused or given up, as opposed to removed by a delete or a clear.
  const lost = new Set<string>();
  let offered: HistoryRecord | null = null;
  let draining = false;
  let disposed = false;
  let stopRetries: (() => void) | null = null;
  // The clear count as last read for a write, while nothing in another tab may have changed it since. Never read
  // for its own sake, so History opens its database only to save.
  let known: number | null = null;
  /** Clears and other tabs' changes heard so far. */
  let changes = 0;
  const read = () =>
    repository.clears().then((clears) => {
      known = clears;
      return clears;
    });

  const showUnsaved = () =>
    batch(() => {
      unsaved.set(offered !== null && settled.get(offered.id) !== true);
      failed.set(offered !== null && lost.has(offered.id));
    });

  const enqueue = (record: HistoryRecord) => {
    if (settled.has(record.id) || queue.some((entry) => entry.record.id === record.id)) return;
    // The head may be mid-write; the oldest behind it goes.
    if (queue.length >= MAX_QUEUED) {
      const dropped = queue.splice(1, 1)[0]!.record.id;
      settled.set(dropped, false);
      lost.add(dropped);
    }
    // A refusal is final and says why; any other failure falls back on the count known to be current.
    queue.push({
      record,
      clears: read().catch((error) => (error instanceof HistoryRefusal ? error : known)),
      queuedAt: changes,
    });
    pending.set(true);
    void drain();
  };

  const settle = (entry: Entry, written: boolean, message: string) =>
    batch(() => {
      queue.shift();
      settled.set(entry.record.id, written);
      if (!written && message) lost.add(entry.record.id);
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
          let clears = await entry.clears;
          if (clears instanceof HistoryRefusal) throw clears;
          if (clears === null) {
            // A count read now is the one it was queued under while nothing has changed History since.
            if (changes !== entry.queuedAt) {
              settle(entry, false, UNORDERED);
              continue;
            }
            clears = await read();
          }
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
    if (change.type === "cleared" || change.type === "elsewhere") changes++;
    if (change.type === "cleared" && known !== null) known++;
    // Another tab may have cleared History: the count is known again at the next write's read.
    if (change.type === "elsewhere") known = null;
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
      // Only a result still waiting or still shown needs its outcome remembered.
      for (const id of settled.keys()) if (id !== record.id) settled.delete(id);
      for (const id of lost) if (id !== record.id) lost.delete(id);
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
    failed,
    warning,
    dispose() {
      disposed = true;
      stopFollowing();
      stopWatching();
      stopRetries?.();
    },
  };
}
