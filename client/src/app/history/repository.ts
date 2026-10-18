// Saved History: IndexedDB "graphite-meter", and notice of each change to it in this tab and others.
import { HISTORY_LIMIT, readHistoryRecord, upgradeRecord, type HistoryRecord } from "./records";

const DATABASE = "graphite-meter";
/** Version 2 held rows of any schema, version 3 schema 5; version 4 holds them upgraded once, in this build's form. */
const VERSION = 4;
const RESULTS = "results";
const META = "meta";
const COMPLETED_AT = "completedAt";
const CLEARS = "clears";
const CHANNEL = "graphite-meter-history";
const OPEN_MS = 5_000;
const BLOCKED_MS = 1_000;
const UNSAVABLE = "Could not save this result in browser storage.";
const FULL = "Browser storage is full. This result was not saved.";
const UNAVAILABLE = "This browser has no storage for History: results are not saved.";
const UNSUPPORTED = "Unsupported History database version. Saved data has not been changed.";
const CLOSED = "History storage was closed before it opened.";
const OLDER = `This result predates the ${HISTORY_LIMIT.toLocaleString("en-US")} newest in History and was not saved.`;

/** A write History will never take; the message says why. */
export class HistoryRefusal extends Error {}

export type HistoryChange =
  | { type: "saved"; record: HistoryRecord }
  /** Rows gone by id: one deleted, or the oldest a save pushed past the limit. */
  | { type: "deleted"; ids: string[] }
  | { type: "cleared" }
  | { type: "malformed-removed" }
  /** Another tab changed History. */
  | { type: "elsewhere" };

const request = <T>(pending: IDBRequest<T>) =>
  new Promise<T>((resolve, reject) => {
    pending.onsuccess = () => resolve(pending.result);
    pending.onerror = () => reject(pending.error ?? new Error("IndexedDB request failed"));
  });

const done = (tx: IDBTransaction) =>
  new Promise<void>((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onerror = tx.onabort = () => reject(tx.error ?? new Error("IndexedDB transaction failed"));
  });

/** The clears ever made in any tab; a corrupt count reads as none, so it never blocks saving. */
const clearCount = (row: unknown): number => {
  const value = (row as { value?: unknown } | undefined)?.value;
  return Number.isSafeInteger(value) && (value as number) > 0 ? (value as number) : 0;
};

/** Walking rows newest first, the id of each readable row past the limit; unreadable rows are never counted. */
function pastLimit(): (row: unknown) => string | null {
  let kept = 0;
  return (row) => {
    const record = readHistoryRecord(row);
    return record && ++kept > HISTORY_LIMIT ? record.id : null;
  };
}

interface OpenRequest {
  blocked: boolean;
  waiter: { opened(db: IDBDatabase): void; failed(error: unknown): void; blocked(): void } | null;
}

/** Creates the database from nothing, or upgrades version 2's or 3's readable rows in place, unreadable ones left as
    they are; any other version is refused untouched. An unawaited answer is closed. */
function openRequest(answered: () => void): OpenRequest {
  const opening = indexedDB.open(DATABASE, VERSION);
  const pending: OpenRequest = { blocked: false, waiter: null };
  // Refused on the abort's error, once the upgrade has unwound: Chromium can hang a page that opens behind it.
  let refusal: Error | undefined;
  opening.onupgradeneeded = ({ oldVersion }) => {
    const upgrade = opening.transaction!;
    if (oldVersion === 2 || oldVersion === 3) {
      const scan = upgrade.objectStore(RESULTS).openCursor();
      scan.onsuccess = () => {
        const cursor = scan.result;
        if (!cursor) return;
        const record = upgradeRecord(cursor.value);
        if (record) cursor.update(record);
        cursor.continue();
      };
      return;
    }
    if (oldVersion !== 0) {
      refusal = new HistoryRefusal(UNSUPPORTED);
      return upgrade.abort();
    }
    const db = opening.result;
    db.createObjectStore(RESULTS, { keyPath: "id" }).createIndex(COMPLETED_AT, COMPLETED_AT, { unique: false });
    db.createObjectStore(META, { keyPath: "key" });
  };
  opening.onsuccess = () => {
    answered();
    if (pending.waiter) pending.waiter.opened(opening.result);
    else opening.result.close();
  };
  opening.onerror = () => {
    answered();
    // A database at a newer version stays so for the page's life.
    const error = opening.error?.name === "VersionError" ? new HistoryRefusal(UNSUPPORTED) : opening.error;
    pending.waiter?.failed(refusal ?? error ?? new Error("IndexedDB open failed"));
  };
  opening.onblocked = () => {
    pending.blocked = true;
    pending.waiter?.blocked();
  };
  return pending;
}

interface Connection {
  db: IDBDatabase;
  drop(): void;
}

export class HistoryRepository {
  #connection: Promise<Connection> | null = null;
  /** Kept until answered: a new request would queue behind it, never hearing `blocked`. */
  #request: OpenRequest | null = null;
  #listeners = new Set<(change: HistoryChange) => void>();
  #channel: BroadcastChannel | null = null;

  async clears(): Promise<number> {
    const tx = await this.#transaction("readonly");
    return clearCount(await request(tx.objectStore(META).get(CLEARS)));
  }

  /** Saves `record` unless History was cleared since `clears` was read, and resolves whether it did. Past the limit
      the oldest readable records go; when that is `record` itself, it is refused. */
  async put(record: HistoryRecord, clears: number): Promise<boolean> {
    // A record History could not read back would only sit in storage as malformed.
    if (!readHistoryRecord(record)) throw new HistoryRefusal(UNSAVABLE);
    const tx = await this.#transaction("readwrite");
    const results = tx.objectStore(RESULTS);
    const stored = tx.objectStore(META).get(CLEARS);
    let written = false;
    let refused: unknown;
    const pruned: string[] = [];
    stored.onsuccess = () => {
      if (clearCount(stored.result) > clears) return;
      try {
        results.put(record);
      } catch (error) {
        refused = error;
        return tx.abort();
      }
      written = true;
      const index = results.index(COMPLETED_AT);
      const total = index.count();
      total.onsuccess = () => {
        // Below the limit no stored value is decoded just to save one result.
        if (total.result <= HISTORY_LIMIT) return;
        const past = pastLimit();
        const scan = index.openCursor(null, "prev");
        scan.onsuccess = () => {
          const cursor = scan.result;
          if (!cursor) return;
          const id = past(cursor.value);
          if (id !== null) {
            cursor.delete();
            pruned.push(id);
          }
          cursor.continue();
        };
      };
    };
    try {
      await done(tx);
    } catch (error) {
      const cause = refused ?? error;
      const name = cause instanceof DOMException ? cause.name : "";
      if (name === "DataCloneError") throw new HistoryRefusal(UNSAVABLE, { cause });
      if (name === "QuotaExceededError") throw new HistoryRefusal(FULL, { cause });
      throw cause;
    }
    const kept = written && !pruned.includes(record.id);
    const gone = pruned.filter((id) => id !== record.id);
    this.#changed(
      ...(gone.length ? [{ type: "deleted", ids: gone } as const] : []),
      ...(kept ? [{ type: "saved", record } as const] : []),
    );
    if (written && !kept) throw new HistoryRefusal(OLDER);
    return written;
  }

  /** The newest 2,000 readable records as `keep` trims them, and how many rows could not be read. An abort stops the
      walk at the next row, never a write. */
  async list<T>(
    signal: AbortSignal,
    keep: (record: HistoryRecord) => T,
  ): Promise<{ records: T[]; malformedCount: number }> {
    signal.throwIfAborted();
    const tx = await this.#transaction("readonly");
    signal.throwIfAborted();
    const store = tx.objectStore(RESULTS);
    const total = request(store.count());
    const records: T[] = [];
    let readable = 0;
    const walked = new Promise<void>((resolve, reject) => {
      const scan = store.index(COMPLETED_AT).openCursor(null, "prev");
      scan.onerror = () => reject(scan.error);
      scan.onsuccess = () => {
        // A read-only transaction nobody continues ends by itself.
        if (signal.aborted) return reject(signal.reason);
        const cursor = scan.result;
        if (!cursor) return resolve();
        const record = readHistoryRecord(cursor.value);
        if (record && ++readable <= HISTORY_LIMIT) records.push(keep(record));
        cursor.continue();
      };
    });
    const [count] = await Promise.all([total, walked]);
    return { records, malformedCount: count - readable };
  }

  async inspect(id: string): Promise<{ status: "ready"; record: HistoryRecord } | { status: "missing" | "malformed" }> {
    const tx = await this.#transaction("readonly");
    const value = await request(tx.objectStore(RESULTS).get(id));
    if (value === undefined) return { status: "missing" };
    const record = readHistoryRecord(value);
    return record ? { status: "ready", record } : { status: "malformed" };
  }

  /** Deletes every row History cannot read and resolves how many went. */
  async removeMalformed(): Promise<number> {
    const tx = await this.#transaction("readwrite");
    let removed = 0;
    const scan = tx.objectStore(RESULTS).openCursor();
    scan.onsuccess = () => {
      const cursor = scan.result;
      if (!cursor) return;
      if (!readHistoryRecord(cursor.value)) {
        cursor.delete();
        removed++;
      }
      cursor.continue();
    };
    await done(tx);
    if (removed) this.#changed({ type: "malformed-removed" });
    return removed;
  }

  async delete(id: string): Promise<void> {
    const tx = await this.#transaction("readwrite");
    tx.objectStore(RESULTS).delete(id);
    await done(tx);
    this.#changed({ type: "deleted", ids: [id] });
  }

  /** Deletes every row, readable or not, and counts the clear: a write queued before it, in any tab, is dropped
      whatever the clocks say. */
  async clear(): Promise<void> {
    const tx = await this.#transaction("readwrite");
    const meta = tx.objectStore(META);
    tx.objectStore(RESULTS).clear();
    const stored = meta.get(CLEARS);
    stored.onsuccess = () => meta.put({ key: CLEARS, value: clearCount(stored.result) + 1 });
    await done(tx);
    this.#changed({ type: "cleared" });
  }

  /** This tab's changes as they land, and other tabs' as `elsewhere`; returns the unsubscriber. A list read while a
      change lands may already hold it, so changes apply by id. */
  onChange(listener: (change: HistoryChange) => void): () => void {
    this.#listeners.add(listener);
    if (!this.#channel && typeof BroadcastChannel === "function") {
      this.#channel = new BroadcastChannel(CHANNEL);
      this.#channel.onmessage = () => this.#notify({ type: "elsewhere" });
    }
    return () => {
      if (!this.#listeners.delete(listener) || this.#listeners.size) return;
      this.#channel?.close();
      this.#channel = null;
    };
  }

  /** Lets go of the connection; the next operation opens it again. */
  close(): void {
    void this.#connection?.then(
      ({ drop }) => drop(),
      () => {},
    );
    this.#connection = null;
  }

  /** Every operation is one transaction over both stores. */
  #transaction(mode: IDBTransactionMode): Promise<IDBTransaction> {
    const connection = (this.#connection ??= this.#connect());
    return connection.then(({ db }) => db.transaction([RESULTS, META], mode));
  }

  /** Opens the database within 5 s, or within 1 s once another version's connection blocks it. */
  #open(): Promise<IDBDatabase> {
    // Nothing brings it back within the page's life, so no write waits for it.
    if (typeof indexedDB === "undefined") return Promise.reject(new HistoryRefusal(UNAVAILABLE));
    let pending: OpenRequest;
    try {
      pending = this.#request ??= openRequest(() => (this.#request = null));
    } catch {
      // Storage the page may not use stays refused for its life.
      return Promise.reject(new HistoryRefusal(UNAVAILABLE));
    }
    const { promise, resolve, reject } = Promise.withResolvers<IDBDatabase>();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const settle = (finish: () => void) => {
      clearTimeout(timer);
      if (pending.waiter === waiter) pending.waiter = null;
      finish();
    };
    const deadline = (ms: number, message: string) => {
      clearTimeout(timer);
      timer = setTimeout(() => settle(() => reject(new Error(message))), ms);
    };
    const waiter: NonNullable<OpenRequest["waiter"]> = {
      opened: (db) => settle(() => resolve(db)),
      failed: (error) => settle(() => reject(error)),
      blocked: () => deadline(BLOCKED_MS, "History is open elsewhere in another version."),
    };
    // A connection let go before it opened hears so now, not at its deadline.
    pending.waiter?.failed(new Error(CLOSED));
    pending.waiter = waiter;
    if (pending.blocked) waiter.blocked();
    else deadline(OPEN_MS, "History storage did not open in time.");
    return promise;
  }

  #connect(): Promise<Connection> {
    const connection: Promise<Connection> = this.#open().then(
      (db) => {
        // Another tab's upgrade never waits on this page, and a hidden page holds nothing open.
        const drop = () => {
          db.close();
          removeEventListener("pagehide", drop);
          if (this.#connection === connection) this.#connection = null;
        };
        db.onversionchange = db.onclose = drop;
        addEventListener("pagehide", drop);
        return { db, drop };
      },
      (error: unknown) => {
        if (this.#connection === connection) this.#connection = null;
        throw error;
      },
    );
    return connection;
  }

  #changed(...changes: HistoryChange[]): void {
    if (!changes.length) return;
    // Any message has History in other tabs read its list again; a channel never hears itself.
    if (typeof BroadcastChannel === "function") {
      const channel = this.#channel ?? new BroadcastChannel(CHANNEL);
      channel.postMessage("");
      if (channel !== this.#channel) channel.close();
    }
    for (const change of changes) this.#notify(change);
  }

  #notify(change: HistoryChange): void {
    for (const listener of this.#listeners) {
      try {
        listener(change);
      } catch (error) {
        reportError(error);
      }
    }
  }
}
