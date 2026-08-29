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
export function sortHistory(
  records: readonly HistoryRecordV1[],
  sort: HistorySort,
  descending = true,
): HistoryRecordV1[] {
  return [...records].sort((a, b) => {
    const av = value(a, sort);
    const bv = value(b, sort);
    const stableTie = () =>
      b.completedAt - a.completedAt || b.id.localeCompare(a.id);
    if (av == null && bv == null) return stableTie();
    if (av == null) return 1;
    if (bv == null) return -1;
    const delta = av - bv;
    return delta === 0 ? stableTie() : descending ? -delta : delta;
  });
}
