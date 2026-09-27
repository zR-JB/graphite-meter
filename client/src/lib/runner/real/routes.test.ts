import { test, expect } from "bun:test";
import { ROUTES } from "../paths";
import { readPin } from "../../test-helpers.testutil";

test("ROUTES matches api/routes.txt", async () => {
  const pinned = (await readPin("routes.txt")).map(([name, path]) => [
    name,
    path,
  ]);
  expect({ ...ROUTES }).toEqual(Object.fromEntries(pinned));
});
