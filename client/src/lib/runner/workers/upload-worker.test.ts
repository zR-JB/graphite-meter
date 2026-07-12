import { test, expect } from "bun:test";
import { recoverableStatus, uploadPoolBytes } from "./upload-worker";

test("uploadPoolBytes: keeps the POST reservoir independent per lane", () => {
  expect(uploadPoolBytes()).toBe(64 * 1024 * 1024);
  expect(uploadPoolBytes(4)).toBe(24 * 1024 * 1024);
  expect(uploadPoolBytes(2)).toBe(16 * 1024 * 1024);
});

test("recoverableStatus: terminal statuses (429/413/503/410) are not recoverable", () => {
  expect(recoverableStatus(429)).toBe(false);
  expect(recoverableStatus(413)).toBe(false);
  expect(recoverableStatus(503)).toBe(false);
  expect(recoverableStatus(410)).toBe(false);
});

test("recoverableStatus: everything else (incl. 500) is treated transient", () => {
  expect(recoverableStatus(500)).toBe(true);
  expect(recoverableStatus(502)).toBe(true);
  expect(recoverableStatus(404)).toBe(true);
  expect(recoverableStatus(0)).toBe(true);
});
