import { expect, test } from "bun:test";
import { createUuid, isUuid } from "./uuid";

test("UUID validation accepts supported versions and rejects malformed identities", () => {
  for (const value of [
    "00000000-0000-4000-8000-000000000127",
    "550e8400-e29b-11d4-a716-446655440000",
    "550E8400-E29B-51D4-B716-446655440000",
  ])
    expect(isUuid(value)).toBe(true);
  for (const value of [
    null,
    127,
    "not-a-uuid",
    "00000000-0000-0000-0000-000000000000",
    "00000000-0000-4000-7000-000000000127",
    "00000000-0000-6000-8000-000000000127",
    "00000000-0000-4000-8000-000000000127-extra",
  ])
    expect(isUuid(value)).toBe(false);
});

test("generated identities satisfy the shared UUID authority", () => {
  expect(isUuid(createUuid())).toBe(true);
});
