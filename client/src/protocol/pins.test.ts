import { expect, test } from "bun:test";
import { envelope, mismatch, PROTOCOL, PROTOCOLS } from "./envelope";
import { rows } from "./pins.testutil";
import { FAILURE_REASONS } from "./reasons";
import { AUTH_ROUTES, ROUTES } from "./routes";

test("routes and failure reasons match their pins", async () => {
  const routes = await rows("routes.txt");
  const paths = (auth: boolean) =>
    Object.fromEntries(
      routes.filter(([, , kind]) => (kind === "auth") === auth).map(([name, path]) => [name, path]),
    );
  expect(paths(false)).toEqual(ROUTES);
  expect(paths(true)).toMatchObject(AUTH_ROUTES);
  expect(Object.fromEntries(await rows("failurereasons.txt"))).toEqual(FAILURE_REASONS);
});

test("the API protocols match their pin, and any document's envelope reads alike", async () => {
  expect(
    (await rows("protocols.txt")).map(([protocol, first]) => [Number(protocol), first]),
  ).toEqual(PROTOCOLS.map((row) => [...row]));
  expect(PROTOCOLS.at(-1)![0]).toBe(PROTOCOL);
  const preflight = (value: unknown) => envelope(value, true);
  expect(preflight({ engineVersion: "0.13.5", lease: {} })).toEqual({
    engineVersion: "0.13.5",
    protocol: 2,
  });
  expect(preflight({ engineVersion: "0.13.4" })?.protocol).toBe(1);
  expect(preflight({ engineVersion: "\u202ex", protocol: 2 })?.engineVersion).toBeNull();
  const unnamed = preflight({ protocol: 3 });
  expect(unnamed).toEqual({ engineVersion: null, protocol: 3 });
  expect(mismatch("S2", unnamed!, "0.13.6")).toStartWith(
    "S2 runs an unknown Graphite Meter release, newer",
  );
  const bad = [0, -1, 2.5, "2", 2 ** 53].map((protocol) => ({ engineVersion: "1", protocol }));
  for (const none of [[], { server: {} }, ...bad]) expect(preflight(none)).toBeNull();
  // Probes from before the envelope carry none.
  expect(envelope({ engineVersion: "0.13.5", clientIp: "x" }, false)).toBeNull();
  expect(envelope({ engineVersion: "0.15.0", protocol: 3 }, false)?.protocol).toBe(3);
  expect(mismatch("S2", { engineVersion: "0.13.5", protocol: 2 }, "0.13.6")).toBeNull();
});
