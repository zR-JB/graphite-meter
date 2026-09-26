import { describe, expect, test } from "bun:test";
import { formatHistoryRate, formatRecentCompletion } from "./format";

describe("history formatting", () => {
  test("rates honor current bit/byte and decimal/binary preferences", () => {
    expect(
      formatHistoryRate(125_000_000, { base: "base10", kind: "bits" }),
    ).toBe("1.00 Gbit/s");
    expect(formatHistoryRate(1_048_576, { base: "base2", kind: "bytes" })).toBe(
      "1.00 MiB/s",
    );
    expect(formatHistoryRate(null, { base: "base10", kind: "bits" })).toBe("—");
  });

  test("recent completions switch to absolute rendering at sixty minutes", () => {
    const now = Date.UTC(2026, 7, 29, 12);
    expect(formatRecentCompletion(now - 30_000, now)).toBe("now");
    expect(formatRecentCompletion(now - 3 * 60_000, now)).toBe("3 min ago");
    expect(formatRecentCompletion(now + 5_000, now)).toBe("now");
    expect(formatRecentCompletion(now - 59_999, now)).toBe("now");
    expect(formatRecentCompletion(now - 60_000, now)).toBe("1 min ago");
    expect(formatRecentCompletion(now - (59 * 60_000 + 59_999), now)).toBe(
      "59 min ago",
    );
    expect(formatRecentCompletion(now - 60 * 60_000, now)).toBeNull();
    expect(formatRecentCompletion(now - 7 * 60 * 60_000, now)).toBeNull();
  });
});
