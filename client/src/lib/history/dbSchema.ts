export const HISTORY_DB = {
  name: "graphite-meter",
  version: 2,
  resultsStore: "results",
  resultKeyPath: "id",
  completedAtIndex: "completedAt",
  metadataStore: "meta",
  metadataKeyPath: "key",
  clearsKey: "clears",
} as const;
