import { HISTORY_LIMIT, isHistoryRecord, type HistoryRecordV1 } from "./types";

export const HISTORY_DB_NAME = "graphite-meter";
export const HISTORY_DB_VERSION = 1;
const STORE = "results";
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
  async put(record: HistoryRecordV1): Promise<void> {
    if (!isHistoryRecord(record)) throw new Error("Invalid history record");
    const db = await this.db();
    const tx = db.transaction(STORE, "readwrite");
    const store = tx.objectStore(STORE);
    store.put(record);
    const index = store.index(INDEX);
    const keysRequest = index.getAllKeys();
    keysRequest.onsuccess = () => {
      const keys = keysRequest.result;
      if (keys.length > HISTORY_LIMIT)
        for (const key of keys.slice(0, keys.length - HISTORY_LIMIT))
          store.delete(key);
    };
    await transactionDone(tx);
  }
  async listWithDiagnostics(): Promise<HistoryListResult> {
    const db = await this.db();
    const values = await request(
      db
        .transaction(STORE, "readonly")
        .objectStore(STORE)
        .index(INDEX)
        .getAll(),
    );
    const records = values.filter(isHistoryRecord);
    return {
      records: retainNewest(records),
      malformedCount: values.length - records.length,
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
  async clear(): Promise<void> {
    const db = await this.db();
    const tx = db.transaction(STORE, "readwrite");
    tx.objectStore(STORE).clear();
    await transactionDone(tx);
  }

  close(): void {
    this.#db?.close();
    this.#db = null;
  }
}

export type HistoryChange = { type: "put" | "delete" | "clear"; id?: string };
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
