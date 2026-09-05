import { test, expect } from "bun:test";
import {
  nextUploadBytes,
  recoverableStatus,
  uploadPoolBytes,
} from "./upload-worker";

const MIN_POST_BYTES = 128 * 1024;
const MAX_POST_BYTES = 10 * 1024 * 1024;

test("uploadPoolBytes: divides one bounded reservoir across lanes", () => {
  expect(uploadPoolBytes(1, 8)).toBe(256 * 1024 * 1024);
  expect(uploadPoolBytes(4, 8)).toBe(64 * 1024 * 1024);
  expect(uploadPoolBytes(4, 2)).toBe(4 * 1024 * 1024);
  expect(uploadPoolBytes(4, 4)).toBe(6 * 1024 * 1024);
  expect(uploadPoolBytes(16, 2)).toBe(2 * 1024 * 1024);
});

test("uploadPoolBytes: a non-positive lane count still sizes one lane", () => {
  expect(uploadPoolBytes(0, 8)).toBe(256 * 1024 * 1024);
  expect(uploadPoolBytes(-2, 8)).toBe(256 * 1024 * 1024);
});

test("uploadPoolBytes: an unknown device gets a bounded reservoir", () => {
  expect(uploadPoolBytes(1)).toBe(128 * 1024 * 1024);
  expect(uploadPoolBytes(4)).toBe(32 * 1024 * 1024);
  expect(uploadPoolBytes(1, undefined, 64 * 1024 * 1024)).toBe(
    64 * 1024 * 1024,
  );
  expect(uploadPoolBytes(128)).toBe(2 * 1024 * 1024);
});

test("recoverableStatus: explicit client and protocol refusals are terminal", () => {
  for (const status of [400, 401, 403, 404, 429, 413, 503, 410])
    expect(recoverableStatus(status)).toBe(false);
});

test("recoverableStatus: network, timeout, and generic server failures retry", () => {
  for (const status of [0, 408, 500, 502])
    expect(recoverableStatus(status)).toBe(true);
});

test("nextUploadBytes grows and shrinks by at most one step", () => {
  expect(nextUploadBytes(MIN_POST_BYTES, 200, 0, MAX_POST_BYTES)).toEqual({
    bytes: 256 * 1024,
    ewma: 655360,
  });
  expect(nextUploadBytes(MAX_POST_BYTES, 100000, 0, MAX_POST_BYTES)).toEqual({
    bytes: 5 * 1024 * 1024,
    ewma: 104857.6,
  });
});

test("nextUploadBytes clamps to the minimum and pool-derived maximum", () => {
  expect(nextUploadBytes(8 * 1024 * 1024, 1, 0, MAX_POST_BYTES).bytes).toBe(
    MAX_POST_BYTES,
  );
  expect(nextUploadBytes(MIN_POST_BYTES, 100000, 0, 256 * 1024).bytes).toBe(
    MIN_POST_BYTES,
  );
});

test("nextUploadBytes applies the 0.3 EWMA weight", () => {
  expect(nextUploadBytes(1000000, 1000, 2000000, MAX_POST_BYTES)).toEqual({
    bytes: 850000,
    ewma: 1700000,
  });
});

test("nextUploadBytes protects size and EWMA on non-positive elapsed time", () => {
  expect(nextUploadBytes(100000, 0, 50000, MAX_POST_BYTES)).toEqual({
    bytes: 100000,
    ewma: 50000,
  });
  expect(nextUploadBytes(100000, -50, 50000, MAX_POST_BYTES)).toEqual({
    bytes: 100000,
    ewma: 50000,
  });
});
