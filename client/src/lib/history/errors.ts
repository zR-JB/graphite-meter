export class InvalidHistoryRecordError extends Error {
  constructor() {
    super("Invalid history record");
    this.name = "InvalidHistoryRecordError";
  }
}

export class StaleHistoryGenerationError extends Error {
  readonly generation: string;

  constructor(generation: string) {
    super(`History generation is stale; current generation is ${generation}`);
    this.name = "StaleHistoryGenerationError";
    this.generation = generation;
  }
}
