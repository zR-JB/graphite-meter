import { expect, test } from "bun:test";
import { failureDetail } from "./failurePresentation";

test("failure presentation hides low-level transport details", () => {
  for (const detail of [
    "TypeError: Failed to fetch",
    "RangeError: Invalid typed array length",
    "download stream 0 failed: Error: NetworkError",
    "HTTP 503",
    "webtransport session did not establish",
  ]) {
    expect(failureDetail(detail)).toBe("Connection lost");
  }
});

test("failure presentation keeps established human detail", () => {
  expect(failureDetail("unknown upload id")).toBe("unknown upload id");
  expect(failureDetail(undefined, "the link dropped")).toBe("the link dropped");
});
