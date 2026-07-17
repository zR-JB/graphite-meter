import { expect, test } from "bun:test";
import { recoverableDownloadStatus } from "./download-worker";

test("admission rejections are terminal for a download lane", () => {
  expect(recoverableDownloadStatus(429)).toBe(false);
  expect(recoverableDownloadStatus(503)).toBe(false);
  expect(recoverableDownloadStatus(500)).toBe(true);
});
