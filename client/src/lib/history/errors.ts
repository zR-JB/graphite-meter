export class InvalidHistoryRecordError extends Error {
  constructor() {
    super("Invalid history record");
    this.name = "InvalidHistoryRecordError";
  }
}
