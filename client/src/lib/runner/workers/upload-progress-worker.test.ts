import { expect, test } from "bun:test";
import { terminalProgressStatus } from "./upload-progress-worker";

test("progress admission and ownership failures are terminal", () => {
  for (const status of [400, 403, 409, 429, 503]) {
    expect(terminalProgressStatus(status)).toBe(true);
  }
  expect(terminalProgressStatus(500)).toBe(false);
  expect(terminalProgressStatus(502)).toBe(false);
});
