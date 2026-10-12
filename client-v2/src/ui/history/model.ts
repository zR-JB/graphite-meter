// History as data: its columns and their units, each row's words, the day groups, the archive's span, how many rows
// show, and how a change to the archive lands in a list already read. Pure, so a 2,000-result archive is worked out
// once per change and never in the DOM code.
import { HISTORY_LIMIT, sortKeys, type HistorySort, type KeyedRecord } from "../../app/history/records";
import type { HistoryChange } from "../../app/history/repository";
import type { HistoryColumn } from "../../app/preferences";
import type { Outcome, RunResult, StageStatus } from "../../engine/run/result";
import { fmtAddedMs, fmtMs, fmtSpeed, MISSING, rateTier, rateUnit, toRateUnit, type Units } from "../../view/format";
import { LATENCY, OUTCOME, STAGE, STAGE_STATE, TIP } from "../../view/words";
import type { IconName } from "../kit/icon";

export const PAGE_ROWS = 50;
const DAY_MS = 86_400_000;

export const COLUMN: Record<HistoryColumn, { label: string; short: string; icon: IconName; tip: string }> = {
  download: {
    ...STAGE.download,
    tip: "Download\nPayload bytes received per second\nMean of the last interval, warmup left out",
  },
  upload: {
    ...STAGE.upload,
    tip: "Upload\nPayload bytes the server received per second\nTimed at the server; queued bytes don't count",
  },
  bidirectional: { ...STAGE.bidirectional, tip: "Bidirectional\nDownload and upload at once, added together" },
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

/** A column's one unit and the value that fills its bars. `tier` is null for latency and for rates never measured. */
export interface Scale {
  tier: number | null;
  peak: number;
  unit: string;
}

/** Each column's unit from every loaded record, not only the rows shown: latency in ms, a rate in the tier of its
    upper median, so one fast outlier never sets it. */
export function columnScales(records: readonly KeyedRecord[], units: Units): Record<HistoryColumn, Scale> {
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
}
export const daysAt = (now: number): Days => ({
  today: new Date(now).toDateString(),
  yesterday: new Date(now - DAY_MS).toDateString(),
});

/** Recent days, then months, so a sparse archive never gets a heading per result. */
function dayHeading(time: number, days: Days): string {
  const day = new Date(time).toDateString();
  return day === days.today ? "Today" : day === days.yesterday ? "Yesterday" : DATES.month.format(time);
}

/** One date when the oldest and newest share a day, else a range that names a shared year once. */
export function span(records: readonly KeyedRecord[]): string {
  if (!records.length) return "";
  let first = Infinity;
  let last = -Infinity;
  for (const { record } of records) {
    first = Math.min(first, record.completedAt);
    last = Math.max(last, record.completedAt);
  }
  const [from, to] = [dateLabel(first), dateLabel(last)];
  if (from === to) return from;
  const sameYear = new Date(first).getFullYear() === new Date(last).getFullYear();
  return `${sameYear ? DATES.day.format(first) : from} – ${to}`;
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
  /** For the `datetime` attribute. */
  iso: string;
  heading: string;
}
export type Row =
  | (RowBase & {
      kind: "result";
      /** The exact time, for the title and the accessible name. */
      exact: string;
      primary: string;
      /** The server, or how many ran. */
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
  /** The list is in date order, under day headings. */
  byDay: boolean;
}

const STATUS_WORD: Record<StageStatus, string> = {
  complete: MISSING,
  partial: STAGE_STATE.partial.label,
  failed: STAGE_STATE.failed.label,
  "not-run": STAGE_STATE.skipped.label,
};

const LOADED = ["download", "upload", "bidirectional"] as const;

/** The load whose median the Loaded column shows: the highest, the first of equals. */
function loadedStage(result: RunResult): (typeof LOADED)[number] | null {
  let worst: (typeof LOADED)[number] | null = null;
  for (const stage of LOADED) {
    const median = result.latencyByStage[stage]?.p50Ms;
    if (median != null && (worst === null || median > result.latencyByStage[worst]!.p50Ms!)) worst = stage;
  }
  return worst;
}

/** Under a rate, the latency its load added; under idle, its jitter; under loaded, whose load it was. */
function note(result: RunResult, column: HistoryColumn): string {
  if (column === "idle") return result.latency?.jitterMs == null ? "" : `jitter ${fmtMs(result.latency.jitterMs)}`;
  if (column === "loaded") {
    const stage = loadedStage(result);
    return stage ? STAGE[stage].short.toLowerCase() : "";
  }
  const added = result.addedLatency?.[column];
  return added == null ? "" : `${fmtAddedMs(added)} ms`;
}

/** Why a value is missing, in the stage's own word; a bidirectional stage that kept one direction says which. */
function missing(result: RunResult, column: HistoryColumn): string {
  if (column === "loaded") return MISSING;
  if (column === "idle") return STATUS_WORD[result.stages.latency];
  if (column === "bidirectional") {
    const down = result.bidirectional?.down?.reportedBytesPerSec;
    const up = result.bidirectional?.up?.reportedBytesPerSec;
    if (down != null && up == null) return "Down only";
    if (up != null && down == null) return "Up only";
  }
  return STATUS_WORD[result.stages[column]];
}

const notePhrase = (column: HistoryColumn, text: string) =>
  column === "idle" ? `${text} ms` : column === "loaded" ? `under ${text}` : `${text} added`;

/** The words that follow the calendar: the heading, and in date order how the time reads. */
function dated(completedAt: number, byDay: boolean, days: Days): { heading: string; primary: string } {
  const time = DATES.time.format(completedAt);
  if (!byDay) return { heading: "", primary: `${dateLabel(completedAt)}, ${time}` };
  const heading = dayHeading(completedAt, days);
  const recent = heading === "Today" || heading === "Yesterday";
  return { heading, primary: recent ? time : `${DATES.day.format(completedAt)}, ${time}` };
}

function resultRow({ record, keys }: KeyedRecord, context: RowContext, days: Days): Row {
  const { result, completedAt, id } = record;
  const { columns, scales, units } = context;
  const exact = DATES.exact.format(completedAt);
  const selection = result.multiServer.selection;
  const where = selection.length > 1 ? `${selection.length} servers` : (selection[0]?.name ?? "");
  const cells = columns.map((column): Cell => {
    const value = keys[column];
    if (value === null) return { column, text: missing(result, column), value, note: "" };
    const { tier } = scales[column];
    const text = tier === null ? fmtMs(value) : fmtSpeed(toRateUnit(value, units, tier));
    return { column, text, value, note: note(result, column) };
  });
  const label = [
    [exact, where, `${OUTCOME[result.outcome].label.toLowerCase()} result`].filter(Boolean).join(", "),
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
    outcome: result.outcome,
    cells,
    label,
  };
}

/** A row's words; a record they cannot be made from becomes an unreadable row. */
function presentRow(keyed: KeyedRecord, context: RowContext, days: Days): Row {
  try {
    return resultRow(keyed, context, days);
  } catch (error) {
    reportError(error);
    const { id, completedAt } = keyed.record;
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

/** Rows worded under one context, kept by id. A row stays the same object until what it shows changes, so its DOM is
    built again only then: across midnight, only the rows whose day words change. */
export function rowPresenter(context: RowContext): (keyed: KeyedRecord, days: Days) => Row {
  const kept = new Map<string, { row: Row; days: Days }>();
  return (keyed, days) => {
    const entry = kept.get(keyed.record.id);
    if (entry?.days === days) return entry.row;
    let row = entry?.row ?? presentRow(keyed, context, days);
    if (entry) {
      const { heading, primary } = dated(row.completedAt, context.byDay, days);
      if (heading !== row.heading || (row.kind === "result" && primary !== row.primary))
        row = row.kind === "result" ? { ...row, heading, primary } : { ...row, heading };
    }
    kept.set(keyed.record.id, { row, days });
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

/** `added` in the list, replacing the same id; at the limit the oldest other record gives way, so a result opened
    past the newest 2,000 shows and the count stays 2,000. */
export function withRecord(records: readonly KeyedRecord[], added: KeyedRecord): KeyedRecord[] {
  const rest = records.filter(({ record }) => record.id !== added.record.id);
  if (rest.length >= HISTORY_LIMIT) {
    let oldest = 0;
    for (let i = 1; i < rest.length; i++)
      if (rest[i]!.record.completedAt < rest[oldest]!.record.completedAt) oldest = i;
    rest.splice(oldest, 1);
  }
  rest.push(added);
  return rest;
}

export interface Listed {
  records: readonly KeyedRecord[];
  /** Rows that could not be read. */
  malformed: number;
}

/** This tab's change applied to a list. Each sets rows by id, so replaying one the list already holds changes
    nothing: a list read while changes landed takes them all again. */
export function applyChange(listed: Listed, change: Exclude<HistoryChange, { type: "elsewhere" }>): Listed {
  switch (change.type) {
    case "saved":
      return { ...listed, records: withRecord(listed.records, sortKeys([change.record])[0]!) };
    case "deleted": {
      const gone = new Set(change.ids);
      return { ...listed, records: listed.records.filter(({ record }) => !gone.has(record.id)) };
    }
    case "cleared":
      return { records: [], malformed: 0 };
    case "malformed-removed":
      return { ...listed, malformed: 0 };
  }
}
