import { expect, test } from "bun:test";
import { stubGlobals } from "./test-helpers.testutil";
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

test("without crypto.randomUUID (plain-HTTP LAN) identities are v4 with RFC variant bits", () => {
  let fill = 0;
  const restore = stubGlobals({
    crypto: {
      getRandomValues: (bytes: Uint8Array) => bytes.fill(fill),
    },
  });
  try {
    expect(createUuid()).toBe("00000000-0000-4000-8000-000000000000");
    fill = 0xff;
    expect(createUuid()).toBe("ffffffff-ffff-4fff-bfff-ffffffffffff");
  } finally {
    restore();
  }
});
