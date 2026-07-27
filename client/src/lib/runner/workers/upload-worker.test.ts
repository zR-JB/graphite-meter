import { test, expect } from "bun:test";
import { recoverableStatus, uploadPoolBytes } from "./upload-worker";

// The reservoir is also the sizer's ceiling, so dividing it is what made upload
// fall off with lane count. A constrained device keeps its own smaller budget.
test("uploadPoolBytes: divides one bounded reservoir across lanes", () => {
  expect(uploadPoolBytes(1, 8)).toBe(256 * 1024 * 1024);
  expect(uploadPoolBytes(4, 8)).toBe(64 * 1024 * 1024);
  expect(uploadPoolBytes(4, 4)).toBe(6 * 1024 * 1024);
  expect(uploadPoolBytes(16, 2)).toBe(2 * 1024 * 1024);
});

// deviceMemory is reported on a fixed ladder, so 2 and 4 sit either side of the
// smallest tier boundary. Their reservoirs only differ at lane counts where the
// pool floor does not swallow the difference.
test("uploadPoolBytes: a 2 GiB device draws on a smaller reservoir than a 4 GiB one", () => {
  expect(uploadPoolBytes(4, 2)).toBe(4 * 1024 * 1024);
  expect(uploadPoolBytes(4, 4)).toBe(6 * 1024 * 1024);
});

// A lane count of zero reaches here from a stage torn down mid-sizing. Dividing
// by it yields an Infinity pool target the builder then tries to allocate.
test("uploadPoolBytes: a non-positive lane count still sizes one lane", () => {
  expect(uploadPoolBytes(0, 8)).toBe(256 * 1024 * 1024);
  expect(uploadPoolBytes(-2, 8)).toBe(256 * 1024 * 1024);
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
