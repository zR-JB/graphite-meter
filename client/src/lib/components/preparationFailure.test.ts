import { expect, test } from "bun:test";
import { BLOCKED, START_FAILED } from "../presentation/vocabulary";
import { preparationFailurePresentation } from "./preparationFailure";

const base = {
  status: "failed" as const,
  throughput: "stale" as const,
  latency: "stale" as const,
};

test("a failed check names only the affected paths, never the transport error", () => {
  for (const [throughput, latency, detail] of [
    ["failed", "stale", "Throughput path is unavailable"],
    ["stale", "failed", "Latency path is unavailable"],
    ["failed", "failed", "Throughput and latency paths are unavailable"],
  ] as const)
    expect(
      preparationFailurePresentation(
        { ...base, throughput, latency },
        "raw transport failure",
      )?.detail,
    ).toBe(detail);
});

test("a refused or failed start keeps its instruction above old path failures; idle shows none", () => {
  const detail = "Open Settings to resolve the selected servers.";
  expect(
    preparationFailurePresentation(
      { status: "blocked", throughput: "failed", latency: "failed" },
      detail,
    ),
  ).toEqual({ headline: BLOCKED, detail });
  expect(preparationFailurePresentation(base, detail)).toEqual({
    headline: START_FAILED,
    detail,
  });
  expect(
    preparationFailurePresentation({ ...base, status: "idle" }, detail),
  ).toBeNull();
});
