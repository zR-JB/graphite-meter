import { test, expect } from "bun:test";
import { recoverableStatus, uploadPoolBytes } from "./upload-worker";

test("uploadPoolBytes: divides one bounded reservoir across lanes", () => {
  expect(uploadPoolBytes(1, 8)).toBe(256 * 1024 * 1024);
  expect(uploadPoolBytes(4, 8)).toBe(64 * 1024 * 1024);
  expect(uploadPoolBytes(4, 4)).toBe(6 * 1024 * 1024);
  expect(uploadPoolBytes(16, 2)).toBe(2 * 1024 * 1024);
});

test("uploadPoolBytes: a 2 GiB device draws on a smaller reservoir than a 4 GiB one", () => {
  expect(uploadPoolBytes(4, 2)).toBe(4 * 1024 * 1024);
  expect(uploadPoolBytes(4, 4)).toBe(6 * 1024 * 1024);
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
