import { expect, test } from "bun:test";
import { preparationFailurePresentation } from "./preparationFailure";

const base = {
  status: "failed" as const,
  throughput: "stale" as const,
  latency: "stale" as const,
};

test("path failure copy names only the affected paths", () => {
  expect(
    preparationFailurePresentation(
      { ...base, throughput: "failed" },
      "throughput probe request failed",
    ),
  ).toEqual({
    headline: "Connection check failed",
    detail: "Throughput path is unavailable",
  });
  expect(
    preparationFailurePresentation(
      { ...base, throughput: "failed", latency: "failed" },
      "raw transport failure",
    ),
  ).toEqual({
    headline: "Connection check failed",
    detail: "Throughput and latency paths are unavailable",
  });
  expect(
    preparationFailurePresentation(
      { ...base, latency: "failed" },
      "raw latency failure",
    )?.detail,
  ).toBe("Latency path is unavailable");
});

test("session coverage failure is not mislabeled as a path failure", () => {
  expect(
    preparationFailurePresentation(base, "Sign in to run this test"),
  ).toEqual({
    headline: "Test cannot start",
    detail: "Sign in to run this test",
  });
});

test("idle and cancellation have no failure presentation", () => {
  expect(
    preparationFailurePresentation({ ...base, status: "idle" }, "stale error"),
  ).toBeNull();
});
