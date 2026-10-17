import { expect, test } from "bun:test";
import { rows } from "./pins.testutil";
import { FAILURE_REASONS } from "./reasons";
import { ROUTES } from "./routes";

test("routes and failure reasons match their pins", async () => {
  expect(Object.fromEntries((await rows("routes.txt")).map(([name, path]) => [name, path]))).toEqual(ROUTES);
  expect(Object.fromEntries(await rows("failurereasons.txt"))).toEqual(FAILURE_REASONS);
});
