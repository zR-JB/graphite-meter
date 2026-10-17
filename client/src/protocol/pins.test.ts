import { expect, test } from "bun:test";
import { rows } from "./pins.testutil";
import { FAILURE_REASONS } from "./reasons";
import { AUTH_ROUTES, ROUTES } from "./routes";

test("routes and failure reasons match their pins", async () => {
  const routes = await rows("routes.txt");
  const paths = (auth: boolean) =>
    Object.fromEntries(routes.filter(([, , kind]) => (kind === "auth") === auth).map(([name, path]) => [name, path]));
  expect(paths(false)).toEqual(ROUTES);
  expect(paths(true)).toMatchObject(AUTH_ROUTES);
  expect(Object.fromEntries(await rows("failurereasons.txt"))).toEqual(FAILURE_REASONS);
});
