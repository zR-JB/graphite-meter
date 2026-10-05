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
    // Refused on the abort's error, once the upgrade has unwound: Chromium can hang a page that opens behind it.
    let refusal: Error | undefined;
    opening.onupgradeneeded = (event) => {
      if (event.oldVersion !== 0) {
        refusal = new Error(
          "Unsupported history database version. Saved data has not been changed.",
        );
        return opening.transaction!.abort();
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
      fail(refusal ?? opening.error ?? new Error("IndexedDB open failed"));
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
      const index = results.index(HISTORY_DB.completedAtIndex);
      const count = index.count();
      count.onsuccess = () => {
        // Below the cap no archive value needs to be cloned or validated just to save one result.
        if (count.result <= HISTORY_LIMIT) return;
        let kept = 0;
        const scan = index.openCursor(null, "prev");
        scan.onsuccess = () => {
          const cursor = scan.result;
          if (!cursor) return;
          // Unreadable rows stay available for diagnostics and explicit clearing.
          if (readHistoryRecord(cursor.value) && ++kept > HISTORY_LIMIT)
            cursor.delete();
          cursor.continue();
        };
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

  async listWithDiagnostics(signal?: AbortSignal): Promise<{
    records: HistoryRecord[];
    malformedCount: number;
  }> {
    signal?.throwIfAborted();
    const tx = await this.#transaction("readonly");
    signal?.throwIfAborted();
    const store = tx.objectStore(HISTORY_DB.resultsStore);
    const total = request(store.count());
    const records: HistoryRecord[] = [];
    let readable = 0;
    // Each cursor delivery yields to the browser: opening an archive never decodes 2,000 results in one task.
    const scanned = new Promise<void>((resolve, reject) => {
      const scan = store
        .index(HISTORY_DB.completedAtIndex)
        .openCursor(null, "prev");
      scan.onerror = () => reject(scan.error);
      scan.onsuccess = () => {
        // Stop obsolete reads without aborting any result-saving transaction.
        // Without another continue(), this read-only transaction finishes.
        if (signal?.aborted) return reject(signal.reason);
        const cursor = scan.result;
        if (!cursor) return resolve();
        const record = readHistoryRecord(cursor.value);
        if (record) {
          readable++;
          if (records.length < HISTORY_LIMIT) records.push(record);
        }
        cursor.continue();
      };
    });
    const [count] = await Promise.all([total, scanned]);
    return {
      records,
      malformedCount: count - readable,
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

  /** Deletes every record that cannot be read and keeps the rest; returns how many went. */
  async removeMalformed(): Promise<number> {
    const tx = await this.#transaction("readwrite");
    const store = tx.objectStore(HISTORY_DB.resultsStore);
    let removed = 0;
    await new Promise<void>((resolve, reject) => {
      const scan = store.openCursor();
      scan.onerror = () => reject(scan.error);
      scan.onsuccess = () => {
        const cursor = scan.result;
        if (!cursor) return resolve();
        if (!readHistoryRecord(cursor.value)) {
          cursor.delete();
          removed++;
        }
        cursor.continue();
      };
    });
    await done(tx);
    return removed;
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
