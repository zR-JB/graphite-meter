// History as data: columns and units, each row's words, day groups, the span, and how a change lands in a read list.
import {
  HISTORY_LIMIT,
  historyEntry,
  type HistoryEntry,
  type HistorySort,
} from "../../app/history/records";
import type { HistoryChange } from "../../app/history/repository";
import type { HistoryColumn } from "../../app/preferences";
import type { Outcome, StageStatus } from "../../engine/run/result";
import {
  fmtAddedMs,
  fmtMs,
  fmtSpeed,
  MISSING,
  rateTier,
  rateUnit,
  toRateUnit,
  type Units,
} from "../../view/format";
import { LATENCY, OUTCOME, STAGE, STAGE_STATE, TIP } from "../../view/words";
import type { IconName } from "../kit/icon";

export const PAGE_ROWS = 50;

export const COLUMN: Record<
  HistoryColumn,
  { label: string; short: string; icon: IconName; tip: string }
> = {
  download: {
    ...STAGE.download,
    tip: "Download\nPayload bytes received per second\nMean of the last interval, warmup left out",
  },
  upload: {
    ...STAGE.upload,
    tip: "Upload\nPayload bytes the server received per second\nTimed at the server; queued bytes do not count",
  },
  bidirectional: {
    ...STAGE.bidirectional,
    tip: "Bidirectional\nDownload and upload at once, added together",
  },
  idle: { ...LATENCY.idle, icon: STAGE.latency.icon, tip: TIP.latency },
  loaded: {
    ...LATENCY.loaded,
    icon: STAGE.latency.icon,
    tip: "Loaded latency\nHighest median round trip during a transfer",
  },
};

export const SORT_LABEL: Record<HistorySort, string> = {
  date: "Date",
  download: COLUMN.download.label,
  upload: COLUMN.upload.label,
  bidirectional: COLUMN.bidirectional.label,
  idle: COLUMN.idle.label,
  loaded: COLUMN.loaded.label,
};

const isLatency = (column: HistoryColumn) => column === "idle" || column === "loaded";

/** The two orders a sort offers, the one it opens with first. */
export function orders(sort: HistorySort): { descending: boolean; label: string }[] {
  if (sort === "date")
    return [
      { descending: true, label: "Newest first" },
      { descending: false, label: "Oldest first" },
    ];
  if (sort === "idle" || sort === "loaded")
    return [
      { descending: false, label: "Lowest first" },
      { descending: true, label: "Highest first" },
    ];
  return [
    { descending: true, label: "Fastest first" },
    { descending: false, label: "Slowest first" },
  ];
}

export function orderHeading(sort: HistorySort, descending: boolean): string {
  const { label } = orders(sort).find((order) => order.descending === descending)!;
  return `${SORT_LABEL[sort]}, ${label.toLowerCase()}`;
}

/** A column's one unit and the value that fills its bars. `tier` is null for latency and for rates never measured. */
export interface Scale {
  tier: number | null;
  peak: number;
  unit: string;
}

/** Each column's unit from every loaded record: latency in ms, a rate in its upper median's tier, so one outlier never sets it. */
export function columnScales(
  records: readonly HistoryEntry[],
  units: Units,
): Record<HistoryColumn, Scale> {
  const scale = (column: HistoryColumn): Scale => {
    const values = new Float64Array(records.length);
    let count = 0;
    for (const { keys } of records) {
      const value = keys[column];
      if (value !== null) values[count++] = value;
    }
    const sorted = values.subarray(0, count).sort();
    const peak = count ? sorted[count - 1]! : 0;
    if (isLatency(column)) return { tier: null, peak, unit: "ms" };
    if (!count) return { tier: null, peak, unit: "" };
    const tier = rateTier(sorted[count >> 1]!, units);
    return { tier, peak, unit: rateUnit(units, tier) };
  };
  return {
    download: scale("download"),
    upload: scale("upload"),
    bidirectional: scale("bidirectional"),
    idle: scale("idle"),
    loaded: scale("loaded"),
  };
}

// The browser's own locale, as everywhere dates show.
const DATES = {
  exact: new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }),
  time: new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" }),
  day: new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }),
  date: new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", year: "numeric" }),
  month: new Intl.DateTimeFormat(undefined, { month: "long", year: "numeric" }),
};

const dateLabel = (time: number) => DATES.date.format(time);

/** The local calendar days a heading names "Today" and "Yesterday", as `Date#toDateString` writes them. */
export interface Days {
  today: string;
  yesterday: string;
  year: number;
}
export function daysAt(now: number): Days {
  const date = new Date(now);
  const today = date.toDateString();
  const year = date.getFullYear();
  // The calendar day before, which a daylight-saving change can make 23 or 25 hours long.
  date.setDate(date.getDate() - 1);
  return { today, yesterday: date.toDateString(), year };
}

/** Recent days, then months, so a sparse archive never gets a heading per result. */
function dayHeading(time: number, days: Days): string {
  const day = new Date(time).toDateString();
  return day === days.today
    ? "Today"
    : day === days.yesterday
      ? "Yesterday"
      : DATES.month.format(time);
}

/** One date when the oldest and newest share a day, else a range that names a shared year once. */
export function span(records: readonly HistoryEntry[]): string {
  if (!records.length) return "";
  let first = Infinity;
  let last = -Infinity;
  for (const { completedAt } of records) {
    first = Math.min(first, completedAt);
    last = Math.max(last, completedAt);
  }
  const [from, to] = [dateLabel(first), dateLabel(last)];
  if (from === to) return from;
  const sameYear = new Date(first).getFullYear() === new Date(last).getFullYear();
  return `${sameYear ? DATES.day.format(first) : from} to ${to}`;
}

export interface Cell {
  column: HistoryColumn;
  text: string;
  /** What its bar draws against the column's peak; null for a missing value, which shows a word and no bar. */
  value: number | null;
  note: string;
}

interface RowBase {
  id: string;
  completedAt: number;
  iso: string;
  heading: string;
}
export type Row =
  | (RowBase & {
      kind: "result";
      /** The exact time, for the title and the accessible name. */
      exact: string;
      primary: string;
      where: string;
      outcome: Outcome;
      cells: Cell[];
      label: string;
    })
  /** A record that passed validation but could not be put into words: it still links to its detail. */
  | (RowBase & { kind: "unreadable"; date: string });

/** What every row's words depend on besides its record and the day. Peaks are not among them: bars draw them. */
export interface RowContext {
  columns: readonly HistoryColumn[];
  scales: Record<HistoryColumn, Pick<Scale, "tier" | "unit">>;
  units: Units;
  byDay: boolean;
}

const STATUS_WORD: Record<StageStatus, string> = {
  complete: MISSING,
  partial: STAGE_STATE.partial.label,
  failed: STAGE_STATE.failed.label,
  "not-run": STAGE_STATE.skipped.label,
};

/** Under a rate, the latency its load added; under idle, its jitter; under loaded, whose load it was. */
function note(entry: HistoryEntry, column: HistoryColumn): string {
  if (column === "idle") return entry.jitterMs == null ? "" : `jitter ${fmtMs(entry.jitterMs)}`;
  if (column === "loaded")
    return entry.loadedStage ? STAGE[entry.loadedStage].short.toLowerCase() : "";
  const added = entry.addedLatency?.[column];
  return added == null ? "" : `${fmtAddedMs(added)} ms`;
}

/** Why a value is missing, in the stage's own word; a bidirectional stage that kept one direction says which. */
function missing(entry: HistoryEntry, column: HistoryColumn): string {
  if (column === "loaded") return MISSING;
  if (column === "idle") return STATUS_WORD[entry.stages.latency];
  if (column === "bidirectional" && entry.lone)
    return entry.lone === "down" ? "Down only" : "Up only";
  return STATUS_WORD[entry.stages[column]];
}

const notePhrase = (column: HistoryColumn, text: string) =>
  column === "idle" ? `${text} ms` : column === "loaded" ? `under ${text}` : `${text} added`;

/** The words that follow the calendar: in date order the heading and time; otherwise the day and time, or a date. */
function dated(
  completedAt: number,
  byDay: boolean,
  days: Days,
): { heading: string; primary: string } {
  const time = DATES.time.format(completedAt);
  const heading = dayHeading(completedAt, days);
  const recent = heading === "Today" || heading === "Yesterday";
  if (byDay)
    return { heading, primary: recent ? time : `${DATES.day.format(completedAt)}, ${time}` };
  if (!recent && new Date(completedAt).getFullYear() !== days.year)
    return { heading: "", primary: dateLabel(completedAt) };
  return { heading: "", primary: `${recent ? heading : DATES.day.format(completedAt)}, ${time}` };
}

function resultRow(entry: HistoryEntry, context: RowContext, days: Days): Row {
  const { completedAt, id, outcome } = entry;
  const { columns, scales, units } = context;
  const exact = DATES.exact.format(completedAt);
  const where = entry.servers > 1 ? `${entry.servers} servers` : entry.server;
  const cells = columns.map((column): Cell => {
    const value = entry.keys[column];
    if (value === null) return { column, text: missing(entry, column), value, note: "" };
    const { tier } = scales[column];
    const text = tier === null ? fmtMs(value) : fmtSpeed(toRateUnit(value, units, tier));
    return { column, text, value, note: note(entry, column) };
  });
  const label = [
    [exact, where, `${OUTCOME[outcome].label.toLowerCase()} result`].filter(Boolean).join(", "),
    ...columns.map((column, index) => {
      const { text, value, note } = cells[index]!;
      const reading = `${SORT_LABEL[column]} ${text}${value === null ? "" : ` ${scales[column].unit}`}`;
      return note ? `${reading}, ${notePhrase(column, note)}` : reading;
    }),
  ].join(". ");
  return {
    kind: "result",
    id,
    completedAt,
    iso: new Date(completedAt).toISOString(),
    ...dated(completedAt, context.byDay, days),
    exact,
    where,
    outcome,
    cells,
    label,
  };
}

/** A row's words; a record they cannot be made from becomes an unreadable row. */
function presentRow(entry: HistoryEntry, context: RowContext, days: Days): Row {
  try {
    return resultRow(entry, context, days);
  } catch (error) {
    reportError(error);
    const { id, completedAt } = entry;
    const { heading } = dated(completedAt, context.byDay, days);
    return {
      kind: "unreadable",
      id,
      completedAt,
      iso: new Date(completedAt).toISOString(),
      heading,
      date: dateLabel(completedAt),
    };
  }
}

/** Rows worded under one context, kept by id; a row stays the same object until what it shows changes. */
export function rowPresenter(context: RowContext): (entry: HistoryEntry, days: Days) => Row {
  const kept = new Map<string, { row: Row; days: Days }>();
  return (listed, days) => {
    // Results deleted or pruned while the list stays open leave their rows here; past twice the archive, start over.
    if (kept.size > 2 * HISTORY_LIMIT) kept.clear();
    const entry = kept.get(listed.id);
    if (entry?.days === days) return entry.row;
    let row = entry?.row ?? presentRow(listed, context, days);
    if (entry) {
      const { heading, primary } = dated(row.completedAt, context.byDay, days);
      if (heading !== row.heading || (row.kind === "result" && primary !== row.primary))
        row = row.kind === "result" ? { ...row, heading, primary } : { ...row, heading };
    }
    kept.set(listed.id, { row, days });
    return row;
  };
}

export interface Group {
  /** Unique even when a heading repeats, as a future-dated result can make it. */
  key: string;
  heading: string;
  rows: Row[];
}

/** Consecutive rows under one heading; a list out of date order is one group without one. */
export function groupRows(rows: readonly Row[]): Group[] {
  const groups: Group[] = [];
  const seen = new Map<string, number>();
  for (const row of rows) {
    const last = groups.at(-1);
    if (last?.heading === row.heading) {
      last.rows.push(row);
      continue;
    }
    const repeat = seen.get(row.heading) ?? 0;
    seen.set(row.heading, repeat + 1);
    groups.push({ key: `${row.heading}\t${repeat}`, heading: row.heading, rows: [row] });
  }
  return groups;
}

/** `added` in the list, replacing the same id; at the limit the oldest other gives way, so the count stays 2,000. */
export function withEntry(records: readonly HistoryEntry[], added: HistoryEntry): HistoryEntry[] {
  const rest = records.filter(({ id }) => id !== added.id);
  if (rest.length >= HISTORY_LIMIT) {
    let oldest = 0;
    for (let i = 1; i < rest.length; i++)
      if (rest[i]!.completedAt < rest[oldest]!.completedAt) oldest = i;
    rest.splice(oldest, 1);
  }
  rest.push(added);
  return rest;
}

export interface Listed {
  records: readonly HistoryEntry[];
  malformed: number;
}

/** This tab's change applied to a list, by id, so replaying one the list already holds changes nothing. */
export function applyChange(
  listed: Listed,
  change: Exclude<HistoryChange, { type: "elsewhere" }>,
): Listed {
  switch (change.type) {
    case "saved":
      return { ...listed, records: withEntry(listed.records, historyEntry(change.record)) };
    case "deleted": {
      const gone = new Set(change.ids);
      return { ...listed, records: listed.records.filter(({ id }) => !gone.has(id)) };
    }
    case "cleared":
      return { records: [], malformed: 0 };
    case "malformed-removed":
      return { ...listed, malformed: 0 };
  }
}
