import { test, expect } from "bun:test";
import {
  bodyPoolBytes,
  recoverableStatus,
  uploadPoolBytes,
} from "./upload-worker";

// The reservoir is also the sizer's ceiling, so dividing it is what made upload
// fall off with lane count. A constrained device keeps its own smaller budget.
test("uploadPoolBytes: divides one bounded reservoir across lanes", () => {
  expect(uploadPoolBytes(1, 8)).toBe(256 * 1024 * 1024);
  expect(uploadPoolBytes(4, 8)).toBe(64 * 1024 * 1024);
  expect(uploadPoolBytes(4, 4)).toBe(6 * 1024 * 1024);
  expect(uploadPoolBytes(16, 2)).toBe(2 * 1024 * 1024);
});

// Only Chromium reports deviceMemory, so an absent value is the Firefox/Safari
// path on any device. It draws on a bounded reservoir instead of the full one.
test("uploadPoolBytes: an unknown device gets a bounded reservoir", () => {
  expect(uploadPoolBytes(1)).toBe(128 * 1024 * 1024);
  expect(uploadPoolBytes(4)).toBe(32 * 1024 * 1024);
  expect(uploadPoolBytes(1, undefined, 64 * 1024 * 1024)).toBe(
    64 * 1024 * 1024,
  );
  expect(uploadPoolBytes(128)).toBe(2 * 1024 * 1024);
});

// A streamed body has no POST size to reserve for, so it cycles a fixed pool
// and neither the lane count nor the device budget may shrink or grow it.
test("bodyPoolBytes: only a sized body draws on the reservoir", () => {
  expect(bodyPoolBytes("stream", 1)).toBe(8 * 1024 * 1024);
  expect(bodyPoolBytes("stream", 16, 2)).toBe(8 * 1024 * 1024);
  expect(bodyPoolBytes("blob", 4, 8)).toBe(64 * 1024 * 1024);
  expect(bodyPoolBytes("blob", 4)).toBe(32 * 1024 * 1024);
  expect(bodyPoolBytes("blob", 4, 4)).toBe(6 * 1024 * 1024);
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
