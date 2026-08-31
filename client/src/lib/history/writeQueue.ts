import { isHistoryRecord, type HistoryRecordV1 } from "./types";
import { InvalidHistoryRecordError } from "./errors";
export type HistoryWrite = (
  record: HistoryRecordV1,
  isCurrent: () => boolean,
  generation: string,
) => Promise<void>;
export type HistoryRemove = (id: string) => Promise<void>;

type Pending = { record: HistoryRecordV1; generation: string };

export class HistoryWriteQueue {
  #pending: Pending[] = [];
  #generation = "";
  #drain = Promise.resolve();
  #scheduled = false;

  constructor(
    private readonly write: HistoryWrite,
    private readonly remove: HistoryRemove,
    private readonly onSaved: (record: HistoryRecordV1) => void,
    private readonly onPermanentFailure: (record: HistoryRecordV1) => void,
    private readonly onTransientFailure: () => void,
  ) {}

  enqueue(record: HistoryRecordV1): boolean {
    if (!isHistoryRecord(record)) {
      this.onPermanentFailure(record);
      return false;
    }
    if (this.#pending.some((pending) => pending.record.id === record.id))
      return true;
    this.#pending.push({ record, generation: this.#generation });
    this.#schedule();
    return true;
  }

  clear(generation: string): void {
    this.#generation = generation;
    this.#pending = [];
  }

  flush(): Promise<void> {
    if (this.#pending.length) this.#schedule();
    return this.#drain;
  }

  #schedule(): void {
    if (this.#scheduled) return;
    this.#scheduled = true;
    this.#drain = this.#drain.then(async () => {
      this.#scheduled = false;
      await this.#run();
    });
  }

  async #run(): Promise<void> {
    while (this.#pending.length) {
      const pending = this.#pending[0];
      if (pending.generation !== this.#generation) {
        this.#pending.shift();
        continue;
      }
      try {
        await this.write(
          pending.record,
          () => pending.generation === this.#generation,
          pending.generation,
        );
        this.#pending.shift();
        if (pending.generation !== this.#generation) {
          await this.remove(pending.record.id).catch(() => undefined);
          continue;
        }
        this.onSaved(pending.record);
      } catch (error) {
        if (error instanceof InvalidHistoryRecordError) {
          this.#pending.shift();
          this.onPermanentFailure(pending.record);
          continue;
        }
        this.onTransientFailure();
        return;
      }
    }
  }
}
