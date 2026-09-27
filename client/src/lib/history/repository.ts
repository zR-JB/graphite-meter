import { HISTORY_LIMIT, readHistoryRecord, type HistoryRecord } from "./types";
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

const clearCount = (row: unknown): number => {
  const value = (row as { value?: unknown } | undefined)?.value;
  return Number.isSafeInteger(value) && (value as number) > 0
    ? (value as number)
    : 0;
};

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

const OPEN_MS = 5_000;
const BLOCKED_MS = 1_000;

function open(): Promise<IDBDatabase> {
  if (typeof indexedDB === "undefined")
    return Promise.reject(new Error("IndexedDB unavailable"));
  return new Promise((resolve, reject) => {
    let settled = false;
    const fail = (error: Error) => {
      settled = true;
      clearTimeout(timer);
      reject(error);
    };
    let timer = setTimeout(
      () => fail(new Error("History storage did not open in time.")),
      OPEN_MS,
    );
    const opening = indexedDB.open(HISTORY_DB.name, HISTORY_DB.version);
    opening.onupgradeneeded = (event) => {
      if (event.oldVersion !== 0) {
        opening.transaction!.abort();
        return fail(
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
      clearTimeout(timer);
      if (settled) return opening.result.close();
      settled = true;
      resolve(opening.result);
    };
    opening.onerror = () =>
      fail(opening.error ?? new Error("IndexedDB open failed"));
    opening.onblocked = () => {
      clearTimeout(timer);
      timer = setTimeout(
        () => fail(new Error("History is open elsewhere in another version.")),
        BLOCKED_MS,
      );
    };
  });
}

export class HistoryRefusal extends Error {}

type Opened = Promise<{ db: IDBDatabase; drop(): void }>;

export class HistoryRepository {
  #db: Opened | null = null;

  #transaction(mode: IDBTransactionMode) {
    // Another version is refused, never upgraded; a hidden page lets go so it never blocks another tab.
    const opened: Opened = (this.#db ??= open().then(
      (db) => {
        const drop = () => {
          db.close();
          globalThis.removeEventListener?.("pagehide", drop);
          if (this.#db === opened) this.#db = null;
        };
        db.onversionchange = db.onclose = drop;
        globalThis.addEventListener?.("pagehide", drop);
        return { db, drop };
      },
      (error) => {
        if (this.#db === opened) this.#db = null;
        throw error;
      },
    ));
    return opened.then(({ db }) =>
      db.transaction([HISTORY_DB.resultsStore, HISTORY_DB.metadataStore], mode),
    );
  }

  async clears(): Promise<number> {
    const tx = await this.#transaction("readonly");
    const value = await request(
      tx.objectStore(HISTORY_DB.metadataStore).get(HISTORY_DB.clearsKey),
    );
    return clearCount(value);
  }

  /** Writes a result queued after `clears` clears unless history was cleared since; false when skipped. */
  async put(record: HistoryRecord, clears: number): Promise<boolean> {
    const tx = await this.#transaction("readwrite");
    const results = tx.objectStore(HISTORY_DB.resultsStore);
    const current = tx
      .objectStore(HISTORY_DB.metadataStore)
      .get(HISTORY_DB.clearsKey);
    let written = false;
    let refused: unknown;
    current.onsuccess = () => {
      if (clearCount(current.result) > clears) return;
      try {
        results.put(record);
      } catch (error) {
        refused = error;
        return tx.abort();
      }
      written = true;
      const values = results.index(HISTORY_DB.completedAtIndex).getAll();
      values.onsuccess = () => {
        const ids = values.result.flatMap(
          (value) => readHistoryRecord(value)?.id ?? [],
        );
        for (const id of ids.slice(0, Math.max(0, ids.length - HISTORY_LIMIT)))
          results.delete(id);
      };
    };
    await done(tx).catch((error: unknown) => {
      const cause = refused ?? error;
      const name = cause instanceof DOMException ? cause.name : "";
      if (name === "DataCloneError")
        throw new HistoryRefusal(
          "This result could not be saved in browser storage.",
          { cause },
        );
      if (name === "QuotaExceededError")
        throw new HistoryRefusal(
          "Browser storage is full. This result was not saved.",
          { cause },
        );
      throw cause;
    });
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
    const records = values.flatMap((value) => readHistoryRecord(value) ?? []);
    return {
      records: records.reverse().slice(0, HISTORY_LIMIT),
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
    const record = readHistoryRecord(value);
    return record ? { status: "ready", record } : { status: "malformed" };
  }

  async delete(id: string): Promise<void> {
    const tx = await this.#transaction("readwrite");
    tx.objectStore(HISTORY_DB.resultsStore).delete(id);
    await done(tx);
  }

  /** Clears every raw value and counts the clear, independent of the wall clock. */
  async clear(): Promise<void> {
    const tx = await this.#transaction("readwrite");
    const meta = tx.objectStore(HISTORY_DB.metadataStore);
    tx.objectStore(HISTORY_DB.resultsStore).clear();
    const current = meta.get(HISTORY_DB.clearsKey);
    current.onsuccess = () =>
      meta.put({
        key: HISTORY_DB.clearsKey,
        value: clearCount(current.result) + 1,
      });
    await done(tx);
  }

  close(): void {
    void this.#db?.then(({ drop }) => drop()).catch(() => {});
    this.#db = null;
  }
}
