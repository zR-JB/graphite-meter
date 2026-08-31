import type { HistoryRecordV1 } from "./types";
export type HistorySort =
  "date" | "download" | "upload" | "bidirectional" | "idle" | "loaded";
export const HISTORY_SORTS: readonly HistorySort[] = [
  "date",
  "download",
  "upload",
  "bidirectional",
  "idle",
  "loaded",
];
export const HISTORY_SORT_LABEL: Record<HistorySort, string> = {
  date: "Date",
  download: "Download",
  upload: "Upload",
  bidirectional: "Bidirectional",
  idle: "Idle",
  loaded: "Loaded",
};
export function naturalDescending(sort: HistorySort): boolean {
  return sort !== "idle" && sort !== "loaded";
}

function value(record: HistoryRecordV1, sort: HistorySort): number | null {
  if (sort === "date") return record.completedAt;
  if (sort === "download")
    return record.stages.download.result?.reportedBytesPerSec ?? null;
  if (sort === "upload")
    return record.stages.upload.result?.reportedBytesPerSec ?? null;
  if (sort === "bidirectional")
    return record.stages.bidirectional.down && record.stages.bidirectional.up
      ? record.stages.bidirectional.down.reportedBytesPerSec +
          record.stages.bidirectional.up.reportedBytesPerSec
      : null;
  if (sort === "idle") return record.stages.latency.result?.reportedMs ?? null;
  // Use the result's user-facing aggregate; do not recombine detail lanes.
  return record.bufferbloat?.loadedMs ?? null;
}

export interface PreparedHistoryRecord {
  record: HistoryRecordV1;
  id: string;
  completedAt: number;
  keys: Record<HistorySort, number | null>;
}

/** Extract every numeric key once when the repository snapshot changes. */
export function prepareHistorySort(
  records: readonly HistoryRecordV1[],
): PreparedHistoryRecord[] {
  return records.map((record) => ({
    record,
    id: record.id,
    completedAt: record.completedAt,
    keys: Object.fromEntries(
      HISTORY_SORTS.map((sort) => [sort, value(record, sort)]),
    ) as Record<HistorySort, number | null>,
  }));
}

export function sortPreparedHistory(
  prepared: readonly PreparedHistoryRecord[],
  sort: HistorySort,
  descending = true,
): HistoryRecordV1[] {
  return [...prepared]
    .sort((a, b) => {
      const av = a.keys[sort];
      const bv = b.keys[sort];
      const stableTie =
        b.completedAt - a.completedAt || b.id.localeCompare(a.id);
      if (av == null && bv == null) return stableTie;
      if (av == null) return 1;
      if (bv == null) return -1;
      const delta = av - bv;
      return delta === 0 ? stableTie : descending ? -delta : delta;
    })
    .map((entry) => entry.record);
}
