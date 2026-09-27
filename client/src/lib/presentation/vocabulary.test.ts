import { expect, test } from "bun:test";
import { FAILURE_REASONS } from "../runner/contract";
import { reasonLabel } from "./vocabulary";
import { readPin } from "../test-helpers.testutil";

test("failure labels match the shared pin", async () => {
  expect(FAILURE_REASONS.map((r) => [r, reasonLabel(r)])).toEqual(
    await readPin("failurereasons.txt"),
  );
});
