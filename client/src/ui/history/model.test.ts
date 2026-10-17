import { expect, test } from "bun:test";
import { HISTORY_LIMIT, historyEntry, type HistoryEntry, type HistoryRecord } from "../../app/history/records";
import type { HistoryChange } from "../../app/history/repository";
import type { RunResult } from "../../engine/run/result";
import { MISSING } from "../../view/format";
import { applyChange, columnScales, daysAt, groupRows, rowPresenter, type Listed, type RowContext } from "./model";

const at = (day: number, hour = 12) => new Date(2026, 7, day, hour).getTime();
const NOW = at(28, 18);
const bits = { base: "base10", kind: "bits" } as const;
const rate = (reportedBytesPerSec: number) => ({ reportedBytesPerSec });
const latency = (p50Ms: number) => ({ p50Ms, probeCount: 20 });

// Only what History's list reads; the rest of a record never reaches it.
function stored(id: string, completedAt: number, result: Partial<Record<keyof RunResult, unknown>> = {}) {
  return {
    schemaVersion: 6,
    id,
    completedAt,
    build: "",
    result: {
      latency: { reportedMs: 12.4, jitterMs: 2.2 },
      latencyByStage: { latency: latency(12.4), download: latency(23.9), upload: latency(30.25), bidirectional: null },
      addedLatency: { download: 11.5, upload: -0.34, bidirectional: null },
      download: rate(25_000_000),
      upload: rate(5_000_000),
      bidirectional: null,
      stages: { latency: "complete", download: "complete", upload: "complete", bidirectional: "not-run" },
      outcome: "complete",
      multiServer: { selection: [{ id: "a", name: "Frankfurt", url: "" }], servers: [], failures: [] },
      ...result,
    },
  } as unknown as HistoryRecord;
}
const saved = (...args: Parameters<typeof stored>): HistoryEntry => historyEntry(stored(...args));

test("a rate column reads in the tier of its upper median, latency in ms, a column without values unitless", () => {
  const cases: [number[], string, number][] = [
    [[250_000_000], "Gbit/s", 250_000_000],
    // A prefix reads from 1.2 of its step: 1 Gbit/s still reads in Mbit/s.
    [[125_000_000], "Mbit/s", 125_000_000],
    // One fast outlier never lifts the column out of the tier most results read in.
    [[1_000_000, 2_000_000, 250_000_000], "Mbit/s", 250_000_000],
    // The upper median of an even count.
    [[1_000_000, 250_000_000], "Gbit/s", 250_000_000],
    [[200, 300], "kbit/s", 300],
  ];
  for (const [rates, unit, peak] of cases) {
    const scales = columnScales(
      rates.map((bps, i) => saved(String(i), NOW, { download: rate(bps) })),
      bits,
    );
    expect(scales.download, `${rates}`).toMatchObject({ unit, peak });
  }
  const none = columnScales([saved("a", NOW, { bidirectional: null })], bits);
  expect(none.bidirectional).toEqual({ tier: null, peak: 0, unit: "" });
  expect(none.idle).toEqual({ tier: null, peak: 12.4, unit: "ms" });
  expect(columnScales([saved("a", NOW)], { base: "base2", kind: "bytes" }).download.unit).toBe("MiB/s");
});

const context = (records: HistoryEntry[], overrides: Partial<RowContext> = {}): RowContext => ({
  columns: ["download", "upload", "bidirectional", "idle", "loaded"],
  scales: columnScales(records, bits),
  units: bits,
  byDay: true,
  ...overrides,
});
const present = (record: HistoryEntry, rowContext: RowContext) => rowPresenter(rowContext)(record, daysAt(NOW));

test("a row's cells read value, note and the word for a missing value", () => {
  const cases: [string, Partial<Record<keyof RunResult, unknown>>, string[]][] = [
    ["complete", {}, ["200.0/+11.5 ms", "40.00/−0.3 ms", "Skipped/", "12.4/jitter 2.2", "30.3/upload"]],
    [
      "one direction survived",
      {
        bidirectional: { down: rate(1_000_000), up: null },
        stages: { latency: "partial", download: "failed", upload: "not-run", bidirectional: "failed" },
        download: null,
        upload: null,
        latency: null,
        latencyByStage: { latency: null, download: null, upload: null, bidirectional: null },
      },
      ["Failed/", "Skipped/", "Down only/", "Partial/", `${MISSING}/`],
    ],
    [
      "both directions",
      { bidirectional: { down: rate(1_000_000), up: rate(2_000_000) }, addedLatency: null },
      ["200.0/", "40.00/", "24.00/", "12.4/jitter 2.2", "30.3/upload"],
    ],
  ];
  for (const [name, result, expected] of cases) {
    const record = saved("a", NOW, result);
    const row = present(record, context([record]));
    if (row.kind !== "result") throw new Error(name);
    expect(
      row.cells.map(({ text, note }) => `${text}/${note}`),
      name,
    ).toEqual(expected);
  }
});

test("a row's accessible name reads each shown value with its unit, in column order", () => {
  const record = saved("a", NOW, { outcome: "partial" });
  const columns = ["download", "idle", "loaded", "bidirectional"] as const;
  const rowContext = context([record], { columns: [...columns] });
  const row = present(record, rowContext);
  if (row.kind !== "result") throw new Error("unreadable");
  const at = row.cells.map(({ text, value, column }) =>
    row.label.indexOf(value === null ? text : `${text} ${rowContext.scales[column].unit}`),
  );
  expect(at.every((index) => index > 0)).toBe(true);
  expect(at).toEqual(at.toSorted((a, b) => a - b));
  const many = saved("b", NOW, { multiServer: { selection: [{}, {}, {}], servers: [], failures: [] } });
  expect(present(many, context([many]))).toMatchObject({ where: "3 servers" });
});

test("in date order rows group under Today, Yesterday, then their month; any other order is one group", () => {
  const records = [at(28, 9), at(28, 1), at(27, 23), at(26), at(3), at(1, 0) - 1].map((time, i) =>
    saved(String(i), time),
  );
  const headings = (byDay: boolean) =>
    groupRows(records.map((record) => present(record, context(records, { byDay })))).map(({ heading, rows }) => [
      heading,
      rows.length,
    ]);
  const [today, yesterday, august, july] = headings(true);
  expect([today, yesterday]).toEqual([
    ["Today", 2],
    ["Yesterday", 1],
  ]);
  expect(august).toEqual([expect.stringContaining("2026"), 2]);
  expect(july?.[1]).toBe(1);
  expect(july?.[0]).not.toBe(august?.[0]);
  expect(headings(false)).toEqual([["", 6]]);
});

test("a heading that returns after another keeps its rows in their own group", () => {
  // A result dated after now (another clock) sorts before Today.
  const records = [saved("future", at(30)), saved("today", at(28)), saved("later", at(29))];
  const groups = groupRows(records.map((record) => present(record, context(records))));
  expect(groups.map(({ rows }) => rows.length)).toEqual([1, 1, 1]);
  expect(new Set(groups.map(({ key }) => key)).size).toBe(3);
});

test("changes that landed while a list was read apply again to it without changing the result", () => {
  const [a, b, c] = ["a", "b", "c"].map((id, i) => stored(id, NOW - i * 60_000)) as [
    HistoryRecord,
    HistoryRecord,
    HistoryRecord,
  ];
  const changes: Exclude<HistoryChange, { type: "elsewhere" }>[] = [
    { type: "saved", record: c },
    { type: "deleted", ids: ["a"] },
    { type: "malformed-removed" },
    { type: "cleared" },
    { type: "saved", record: b },
  ];
  const replay = (from: Listed, list: typeof changes) => list.reduce(applyChange, from);
  const before: Listed = { records: [historyEntry(a)], malformed: 3 };
  // A read that saw any prefix of the changes ends where the list that followed them all does.
  for (let seen = 0; seen <= changes.length; seen++) {
    const read = replay(before, changes.slice(0, seen));
    const ids = (listed: Listed) => listed.records.map(({ id }) => id).sort();
    expect(ids(replay(read, changes)), `read after ${seen}`).toEqual(["b"]);
    expect(replay(read, changes).malformed).toBe(0);
  }
});

test("a result added at the limit replaces the oldest other, so the count stays at the limit", () => {
  const full = Array.from({ length: HISTORY_LIMIT }, (_, i) => saved(String(i), NOW - i));
  const old = stored("old", NOW - 1e9);
  const { records } = applyChange({ records: full, malformed: 0 }, { type: "saved", record: old });
  expect(records).toHaveLength(HISTORY_LIMIT);
  expect(records.some(({ id }) => id === "old")).toBe(true);
  expect(records.some(({ id }) => id === String(HISTORY_LIMIT - 1))).toBe(false);
});

test("a row stays the same object across a day unless its day words change", () => {
  const records = [saved("today", at(28, 9)), saved("yesterday", at(27, 9)), saved("older", at(3))];
  for (const byDay of [true, false]) {
    const row = rowPresenter(context(records, { byDay }));
    const before = daysAt(NOW);
    const first = records.map((record) => row(record, before));
    expect(
      records.map((record, i) => row(record, before) === first[i]),
      "same day",
    ).toEqual([true, true, true]);
    const next = records.map((record) => row(record, daysAt(at(29, 9))));
    expect(
      next.map((each, i) => each === first[i]),
      `${byDay}`,
    ).toEqual(byDay ? [false, false, true] : [true, true, true]);
    if (byDay) expect(next.map(({ heading }) => heading).slice(0, 1)).toEqual(["Yesterday"]);
  }
});

test("Yesterday is the calendar day before, even after a 23-hour day", () => {
  const zone = process.env.TZ;
  process.env.TZ = "America/New_York";
  try {
    // 00:30 on the day after the spring-forward change, which lasted 23 hours.
    const now = new Date(2026, 2, 9, 0, 30).getTime();
    expect(daysAt(now)).toEqual({ today: "Mon Mar 09 2026", yesterday: "Sun Mar 08 2026" });
  } finally {
    process.env.TZ = zone;
  }
});
