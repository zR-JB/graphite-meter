import type { LatencyBucket, Phase } from "../runner/contract";

/** Incremental phase index for append-only latency history. A structural
 * revision explicitly invalidates the index so replacements and ordered
 * insertions cannot leave chart or hover consumers on stale bucket objects. */
export class LatencyPhaseIndex {
  #revision = -1;
  #indexed = 0;
  #lastIndexed: LatencyBucket | undefined;
  #byPhase = new Map<Phase, LatencyBucket[]>();

  clear(): void {
    this.#revision = -1;
    this.#indexed = 0;
    this.#lastIndexed = undefined;
    this.#byPhase.clear();
  }

  update(history: readonly LatencyBucket[], revision: number): void {
    if (
      revision !== this.#revision ||
      history.length < this.#indexed ||
      (history.length === this.#indexed && history.at(-1) !== this.#lastIndexed)
    ) {
      this.#indexed = 0;
      this.#byPhase.clear();
    }

    for (let i = this.#indexed; i < history.length; i++) {
      const sample = history[i];
      const lane = this.#byPhase.get(sample.phase) ?? [];
      if (!lane.length) this.#byPhase.set(sample.phase, lane);
      lane.push(sample);
    }
    this.#revision = revision;
    this.#indexed = history.length;
    this.#lastIndexed = history.at(-1);
  }

  values(): IterableIterator<LatencyBucket[]> {
    return this.#byPhase.values();
  }
}
