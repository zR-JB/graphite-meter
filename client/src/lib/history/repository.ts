import { HISTORY_LIMIT, isHistoryRecord, type HistoryRecordV1 } from "./types";
import {
  InvalidHistoryRecordError,
  StaleHistoryGenerationError,
} from "./errors";
import {
  currentHistoryGeneration,
  nextHistoryGeneration,
  restoreHistoryGeneration,
} from "./changes";
export {
  broadcastHistory,
  historyChanges,
  type HistoryChange,
} from "./changes";
export {
  InvalidHistoryRecordError,
  StaleHistoryGenerationError,
} from "./errors";

export const HISTORY_DB_NAME = "graphite-meter";
export const HISTORY_DB_VERSION = 2;
const STORE = "results";
const META = "meta";
const INDEX = "completedAt";

export type HistoryListResult = {
  records: HistoryRecordV1[];
  malformedCount: number;
};

export type HistoryEntryResult =
  | { status: "ready"; record: HistoryRecordV1 }
  | { status: "missing" | "malformed" };

export function retainNewest(
  records: readonly HistoryRecordV1[],
): HistoryRecordV1[] {
  return [...records]
    .sort((a, b) => b.completedAt - a.completedAt || b.id.localeCompare(a.id))
    .slice(0, HISTORY_LIMIT);
}

function request<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () =>
      reject(request.error ?? new Error("IndexedDB request failed"));
  });
}
function transactionDone(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onerror = () =>
      reject(transaction.error ?? new Error("IndexedDB transaction failed"));
    transaction.onabort = () =>
      reject(transaction.error ?? new Error("IndexedDB transaction aborted"));
  });
}
export function openHistoryDB(): Promise<IDBDatabase> {
  if (typeof indexedDB === "undefined")
    return Promise.reject(new Error("IndexedDB unavailable"));
  return new Promise((resolve, reject) => {
    const opening = indexedDB.open(HISTORY_DB_NAME, HISTORY_DB_VERSION);
    opening.onupgradeneeded = () => {
      const db = opening.result;
      const store = db.objectStoreNames.contains(STORE)
        ? opening.transaction!.objectStore(STORE)
        : db.createObjectStore(STORE, { keyPath: "id" });
      if (!store.indexNames.contains(INDEX))
        store.createIndex(INDEX, INDEX, { unique: false });
      if (!db.objectStoreNames.contains(META))
        db.createObjectStore(META, { keyPath: "key" });
    };
    opening.onsuccess = () => {
      opening.result.onversionchange = () => opening.result.close();
      resolve(opening.result);
    };
    opening.onerror = () =>
      reject(opening.error ?? new Error("IndexedDB open failed"));
    opening.onblocked = () => reject(new Error("IndexedDB open blocked"));
  });
}
export class HistoryRepository {
  #db: IDBDatabase | null = null;
  async db(): Promise<IDBDatabase> {
    return (this.#db ??= await openHistoryDB());
  }
  async put(
    record: HistoryRecordV1,
    generation = currentHistoryGeneration(),
  ): Promise<void> {
    if (!isHistoryRecord(record)) throw new InvalidHistoryRecordError();
    const db = await this.db();
    const tx = db.transaction([STORE, META], "readwrite");
    const store = tx.objectStore(STORE);
    const metadata = tx.objectStore(META);
    const generationRequest = metadata.get("generation");
    let staleGeneration: string | undefined;
    generationRequest.onsuccess = () => {
      const durableGeneration = generationRequest.result?.value;
      if (
        generation &&
        generationRequest.result &&
        durableGeneration !== generation
      ) {
        staleGeneration = durableGeneration ?? "";
        return;
      }
      if (!generationRequest.result)
        metadata.put({ key: "generation", value: generation });
      store.put(record);
      const keysRequest = store.index(INDEX).getAllKeys();
      keysRequest.onsuccess = () => {
        const keys = keysRequest.result;
        if (keys.length > HISTORY_LIMIT)
          for (const key of keys.slice(0, keys.length - HISTORY_LIMIT))
            store.delete(key);
      };
    };
    await transactionDone(tx);
    if (staleGeneration !== undefined)
      throw new StaleHistoryGenerationError(staleGeneration);
  }
  async listWithDiagnostics(): Promise<HistoryListResult> {
    const db = await this.db();
    const tx = db.transaction(STORE, "readonly");
    const store = tx.objectStore(STORE);
    const rawCount = request(store.count());
    let indexedCount = 0;
    let malformedIndexedCount = 0;
    const records: HistoryRecordV1[] = [];
    const scan = new Promise<void>((resolve, reject) => {
      const cursorRequest = store.index(INDEX).openCursor(null, "prev");
      cursorRequest.onerror = () =>
        reject(
          cursorRequest.error ?? new Error("IndexedDB cursor request failed"),
        );
      cursorRequest.onsuccess = () => {
        const cursor = cursorRequest.result;
        if (!cursor) {
          resolve();
          return;
        }
        indexedCount += 1;
        if (isHistoryRecord(cursor.value)) {
          if (records.length < HISTORY_LIMIT) records.push(cursor.value);
        } else {
          malformedIndexedCount += 1;
        }
        cursor.continue();
      };
    });
    const [totalCount] = await Promise.all([rawCount, scan]);
    return {
      records: retainNewest(records),
      malformedCount: totalCount - indexedCount + malformedIndexedCount,
    };
  }
  async inspect(id: string): Promise<HistoryEntryResult> {
    const db = await this.db();
    const value = await request(
      db.transaction(STORE, "readonly").objectStore(STORE).get(id),
    );
    if (value === undefined) return { status: "missing" };
    return isHistoryRecord(value)
      ? { status: "ready", record: value }
      : { status: "malformed" };
  }
  async delete(id: string): Promise<void> {
    const db = await this.db();
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).delete(id);
    await transactionDone(tx);
  }
  /** Clear every raw value, including entries that fail the history schema. */
  async clear(): Promise<string> {
    const previousGeneration = currentHistoryGeneration();
    const generation = nextHistoryGeneration();
    try {
      const db = await this.db();
      const tx = db.transaction([STORE, META], "readwrite");
      tx.objectStore(STORE).clear();
      tx.objectStore(META).put({ key: "generation", value: generation });
      await transactionDone(tx);
    } catch (error) {
      restoreHistoryGeneration(previousGeneration);
      throw error;
    }
    return generation;
  }

  close(): void {
    this.#db?.close();
    this.#db = null;
  }
}
