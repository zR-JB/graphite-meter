import { HISTORY_LIMIT, isHistoryRecord, type HistoryRecord } from "./types";
import { HISTORY_DB } from "./dbSchema";

const CHANNEL = "graphite-meter-history";

/** Every view in every tab reloads after a history change, except the view that made it. */
export function onHistoryChanged(listener: () => void, self = ""): () => void {
  if (typeof BroadcastChannel === "undefined") return () => {};
  const channel = new BroadcastChannel(CHANNEL);
  channel.onmessage = (event) => {
    if (!self || event.data !== self) listener();
  };
  return () => channel.close();
}

export function announceHistoryChanged(source = ""): void {
  if (typeof BroadcastChannel === "undefined") return;
  const channel = new BroadcastChannel(CHANNEL);
  channel.postMessage(source);
  channel.close();
}

export function retainNewest(
  records: readonly HistoryRecord[],
): HistoryRecord[] {
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

function done(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    const fail = () =>
      reject(transaction.error ?? new Error("IndexedDB transaction failed"));
    transaction.oncomplete = () => resolve();
    transaction.onerror = transaction.onabort = fail;
  });
}

function open(): Promise<IDBDatabase> {
  if (typeof indexedDB === "undefined")
    return Promise.reject(new Error("IndexedDB unavailable"));
  return new Promise((resolve, reject) => {
    const opening = indexedDB.open(HISTORY_DB.name, HISTORY_DB.version);
    opening.onupgradeneeded = (event) => {
      if (event.oldVersion !== 0) {
        opening.transaction!.abort();
        return reject(
          new Error(
            "Unsupported history database version. Saved data has not been changed.",
          ),
        );
      }
      const db = opening.result;
      db.createObjectStore(HISTORY_DB.resultsStore, {
        keyPath: HISTORY_DB.resultKeyPath,
      }).createIndex(HISTORY_DB.completedAtIndex, HISTORY_DB.completedAtIndex, {
        unique: false,
      });
      db.createObjectStore(HISTORY_DB.metadataStore, {
        keyPath: HISTORY_DB.metadataKeyPath,
      });
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
  #db: Promise<IDBDatabase> | null = null;

  #transaction(mode: IDBTransactionMode) {
    // A refused open is retried by the next request.
    this.#db ??= open().catch((error) => {
      this.#db = null;
      throw error;
    });
    return this.#db.then((db) =>
      db.transaction([HISTORY_DB.resultsStore, HISTORY_DB.metadataStore], mode),
    );
  }

  /** Writes a result unless history was cleared after it completed; false when skipped. */
  async put(record: HistoryRecord): Promise<boolean> {
    const tx = await this.#transaction("readwrite");
    const results = tx.objectStore(HISTORY_DB.resultsStore);
    const watermark = tx
      .objectStore(HISTORY_DB.metadataStore)
      .get(HISTORY_DB.clearedAtKey);
    let written = false;
    watermark.onsuccess = () => {
      const clearedAt = watermark.result?.value;
      if (typeof clearedAt === "number" && record.completedAt <= clearedAt)
        return;
      written = true;
      results.put(record);
      const keys = results.index(HISTORY_DB.completedAtIndex).getAllKeys();
      keys.onsuccess = () => {
        for (const key of keys.result.slice(
          0,
          Math.max(0, keys.result.length - HISTORY_LIMIT),
        ))
          results.delete(key);
      };
    };
    await done(tx);
    return written;
  }

  async listWithDiagnostics(): Promise<{
    records: HistoryRecord[];
    malformedCount: number;
  }> {
    const store = (await this.#transaction("readonly")).objectStore(
      HISTORY_DB.resultsStore,
    );
    const values: unknown[] = await request(
      store.index(HISTORY_DB.completedAtIndex).getAll(),
    );
    const total = await request(store.count());
    const records = values.filter(isHistoryRecord);
    return {
      records: retainNewest(records),
      malformedCount: total - records.length,
    };
  }

  async inspect(
    id: string,
  ): Promise<
    | { status: "ready"; record: HistoryRecord }
    | { status: "missing" | "malformed" }
  > {
    const tx = await this.#transaction("readonly");
    const value = await request(
      tx.objectStore(HISTORY_DB.resultsStore).get(id),
    );
    if (value === undefined) return { status: "missing" };
    return isHistoryRecord(value)
      ? { status: "ready", record: value }
      : { status: "malformed" };
  }

  async delete(id: string): Promise<void> {
    const tx = await this.#transaction("readwrite");
    tx.objectStore(HISTORY_DB.resultsStore).delete(id);
    await done(tx);
  }

  /** Clears every raw value; results completed before now are never written again. */
  async clear(): Promise<void> {
    const tx = await this.#transaction("readwrite");
    tx.objectStore(HISTORY_DB.resultsStore).clear();
    tx.objectStore(HISTORY_DB.metadataStore).put({
      key: HISTORY_DB.clearedAtKey,
      value: Date.now(),
    });
    await done(tx);
  }

  close(): void {
    void this.#db?.then((db) => db.close()).catch(() => {});
    this.#db = null;
  }
}
