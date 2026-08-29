import { describe, expect, test } from "bun:test";
import {
  formatDuration,
  formatHistoryBytes,
  formatHistoryRate,
  formatRecentCompletion,
} from "./format";

describe("history formatting", () => {
  test("duration changes scale without implying excessive precision", () => {
    expect(formatDuration(0)).toBe("0 ms");
    expect(formatDuration(999)).toBe("999 ms");
    expect(formatDuration(1_000)).toBe("1.0 s");
    expect(formatDuration(9_949)).toBe("9.9 s");
    expect(formatDuration(9_950)).toBe("9.9 s");
    expect(formatDuration(10_000)).toBe("10 s");
    expect(formatDuration(59_600)).toBe("60 s");
    expect(formatDuration(60_000)).toBe("1 min");
    expect(formatDuration(65_000)).toBe("1 min 5 s");
    expect(formatDuration(3_599_000)).toBe("59 min 59 s");
    expect(formatDuration(3_600_000)).toBe("1 h");
    expect(formatDuration(3_690_000)).toBe("1 h 2 min");
  });

  test("rates honor current bit/byte and decimal/binary preferences", () => {
    expect(
      formatHistoryRate(125_000_000, { base: "base10", kind: "bits" }),
    ).toBe("1.00 Gbit/s");
    expect(formatHistoryRate(1_048_576, { base: "base2", kind: "bytes" })).toBe(
      "1.00 MiB/s",
    );
    expect(formatHistoryRate(null, { base: "base10", kind: "bits" })).toBe(
      "Unavailable",
    );
  });

  test("transferred bytes use the user's scaling base", () => {
    expect(formatHistoryBytes(1_000_000, "base10")).toBe("1.0 MB");
    expect(formatHistoryBytes(1_048_576, "base2")).toBe("1.0 MiB");
  });

  test("recent completions are relative without a background timer", () => {
    const now = Date.UTC(2026, 7, 29, 12);
    expect(formatRecentCompletion(now - 30_000, now)).toBe("now");
    expect(formatRecentCompletion(now - 3 * 60_000, now)).toBe("3 min ago");
    expect(formatRecentCompletion(now - 60 * 60_000, now)).toBe("1 hr ago");
    expect(formatRecentCompletion(now - 23 * 60 * 60_000, now)).toBe(
      "23 hr ago",
    );
    expect(formatRecentCompletion(now - 24 * 60 * 60_000, now)).toBeNull();
    expect(formatRecentCompletion(now + 5_000, now)).toBe("now");
  });
});
