import { bidirectionalResultPresentation } from "../presentation/bidirectionalResult";
import type { HistoryRecord } from "./types";
import { LATENCY_POPULATION, STAGE } from "../presentation/vocabulary";
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
  download: STAGE.download.label,
  upload: STAGE.upload.label,
  bidirectional: STAGE.bidirectional.label,
  idle: LATENCY_POPULATION.latency.label,
  loaded: "Loaded latency",
};
export function naturalDescending(sort: HistorySort): boolean {
  return sort !== "idle" && sort !== "loaded";
}

export function historyMetrics({
  completedAt,
  result,
}: HistoryRecord): Record<HistorySort, number | null> {
  // The highest loaded median, as the detail lanes centre on it, with or without idle latency.
  const loaded = (["download", "upload", "bidirectional"] as const).flatMap(
    (stage) => result.latencyByStage[stage]?.p50Ms ?? [],
  );
  return {
    date: completedAt,
    download: result.download?.reportedBytesPerSec ?? null,
    upload: result.upload?.reportedBytesPerSec ?? null,
    bidirectional: bidirectionalResultPresentation(
      result.bidirectional?.down?.reportedBytesPerSec,
      result.bidirectional?.up?.reportedBytesPerSec,
    ).combinedBytesPerSec,
    idle: result.latency?.reportedMs ?? null,
    loaded: loaded.length ? Math.max(...loaded) : null,
  };
}

interface PreparedHistoryRecord {
  record: HistoryRecord;
  id: string;
  completedAt: number;
  keys: Record<HistorySort, number | null>;
}

/** Extract every numeric key once when the repository snapshot changes. */
export function prepareHistorySort(
  records: readonly HistoryRecord[],
): PreparedHistoryRecord[] {
  return records.map((record) => ({
    record,
    id: record.id,
    completedAt: record.completedAt,
    keys: historyMetrics(record),
  }));
}

export function sortPreparedHistory(
  prepared: readonly PreparedHistoryRecord[],
  sort: HistorySort,
  descending = true,
): HistoryRecord[] {
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
