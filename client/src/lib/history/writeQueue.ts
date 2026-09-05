import { isHistoryRecord, type HistoryRecord } from "./types";
import {
  InvalidHistoryRecordError,
  StaleHistoryGenerationError,
} from "./errors";
import { currentHistoryGeneration, isRepairHistoryGeneration } from "./changes";
type HistoryWrite = (
  record: HistoryRecord,
  isCurrent: () => boolean,
  generation: string,
) => Promise<void>;
type HistoryRemove = (id: string) => Promise<void>;

type Pending = { record: HistoryRecord; generation: string };

export class HistoryWriteQueue {
  #pending: Pending[] = [];
  #generation = "";
  #drain = Promise.resolve();
  #scheduled = false;
  #disposed = false;

  constructor(
    private readonly write: HistoryWrite,
    private readonly remove: HistoryRemove,
    private readonly onSaved: (record: HistoryRecord) => void,
    private readonly onPermanentFailure: (record: HistoryRecord) => void,
    private readonly onTransientFailure: () => void,
  ) {}

  enqueue(record: HistoryRecord): boolean {
    if (this.#disposed) return false;
    const observedGeneration = currentHistoryGeneration();
    if (observedGeneration && observedGeneration !== this.#generation) {
      if (isRepairHistoryGeneration(observedGeneration))
        this.#resynchronize(observedGeneration);
      else this.clear(observedGeneration);
    }
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

  dispose(): void {
    this.#disposed = true;
    this.#pending = [];
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

  #resynchronize(generation: string): void {
    this.#generation = generation;
    for (const pending of this.#pending) pending.generation = generation;
  }

  async #run(): Promise<void> {
    while (!this.#disposed && this.#pending.length) {
      const pending = this.#pending[0];
      if (pending.generation !== this.#generation) {
        this.#pending.shift();
        continue;
      }
      try {
        await this.write(
          pending.record,
          () => !this.#disposed && pending.generation === this.#generation,
          pending.generation,
        );
        if (this.#disposed) return;
        this.#pending.shift();
        if (pending.generation !== this.#generation) {
          await this.remove(pending.record.id).catch(() => undefined);
          continue;
        }
        this.onSaved(pending.record);
      } catch (error) {
        if (this.#disposed) return;
        if (error instanceof StaleHistoryGenerationError) {
          if (pending.generation !== this.#generation) continue;
          const observedGeneration = currentHistoryGeneration();
          const generation =
            observedGeneration && observedGeneration !== pending.generation
              ? observedGeneration
              : error.generation;
          if (isRepairHistoryGeneration(generation))
            this.#resynchronize(generation);
          else this.clear(generation);
          continue;
        }
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
