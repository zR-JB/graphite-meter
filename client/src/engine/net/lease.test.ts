import { afterEach, expect, test } from "bun:test";
import { fakeNet, granted, publicAuth, type Answer } from "./fakes.testutil";
import { createLease, LeaseRefused } from "./lease";

const PEER = "https://peer.example";
const entry = { id: "peer", url: PEER, name: "Peer", additionalOrigins: [] };
let net: ReturnType<typeof fakeNet> | null = null;
afterEach(() => net?.restore());

/** A lease of the peer whose `/lease` answers come from `answer`, and what each request asked. */
function leased(answer: (init: RequestInit) => Answer = granted) {
  net = fakeNet(
    () => ({ status: 404 }),
    (_, init) => answer(init),
  );
  const asked = () =>
    net!.requests.map(({ url, init }) => ({
      method: init.method,
      url: url.href,
      lease: new Headers(init.headers).get("Graphite-Meter-Lease"),
      body: init.body ? JSON.parse(String(init.body)) : null,
    }));
  return { lease: createLease(PEER, publicAuth(entry)), asked };
}

test("a lease holds the bus and each need until released, presented on requests and in socket URLs, then ends", async () => {
  const { lease, asked } = leased();
  expect(lease.socket(`${PEER}/ws/ping`)).toBe(`${PEER}/ws/ping`);
  const signal = new AbortController().signal;
  const run = await lease.hold({ measurements: 10, sessions: 2 }, signal, 2);
  const check = await lease.hold({ measurements: 1, sessions: 1 }, signal);
  expect(lease.request(`${PEER}/download?bytes=1`, "GET").headers).toEqual({
    "Graphite-Meter-Lease": "gml_t",
  });
  expect(lease.socket("wss://peer.example/ws/ping")).toBe("wss://peer.example/ws/ping?lease=gml_t");
  expect(lease.socket(`${PEER}/wt/upload?id=u`)).toBe(`${PEER}/wt/upload?id=u&lease=gml_t`);
  const ids = [await lease.upload(signal), await lease.upload(signal)];
  expect(new Set(ids).size).toBe(2);
  check();
  check();
  await net!.advance();
  run();
  lease.end();
  await net!.advance();
  const at = `${PEER}/lease`;
  expect(asked()).toEqual([
    { method: "POST", url: at, lease: null, body: { measurements: 11, sessions: 2, uploads: 2 } },
    {
      method: "POST",
      url: at,
      lease: "gml_t",
      body: { measurements: 12, sessions: 3, uploads: 0 },
    },
    {
      method: "POST",
      url: at,
      lease: "gml_t",
      body: { measurements: 11, sessions: 2, uploads: 0 },
    },
    {
      method: "POST",
      url: at,
      lease: "gml_t",
      body: { measurements: 1, sessions: 0, uploads: 0 },
    },
    { method: "DELETE", url: at, lease: "gml_t", body: null },
  ]);
  expect(lease.socket("wss://peer.example/ws/ping")).toBe("wss://peer.example/ws/ping");
});

test("a refusal names the share or the capacity and holds nothing more, and a later id is minted on demand", async () => {
  const answers: Answer[] = [
    { status: 503, headers: { "Graphite-Meter-Refusal": "server-capacity" } },
    { status: 429 },
    { status: 500 },
  ];
  const { lease, asked } = leased((init) => answers.shift() ?? granted(init));
  const signal = new AbortController().signal;
  const need = { measurements: 4, sessions: 0 };
  const refusals = [];
  for (let attempt = 0; attempt < 3; attempt++)
    refusals.push(await lease.hold(need, signal).catch((error: Error) => error));
  expect(refusals.map((error) => error instanceof LeaseRefused && error.reason)).toEqual([
    "server-capacity",
    "client-limit",
    false,
  ]);
  expect(refusals.map((error) => (error as Error).message)).toEqual([
    "Server at capacity",
    "Limit reached for your address or sign-in",
    "The server did not admit the test (HTTP 500)",
  ]);
  await lease.renew();
  expect(await lease.upload(signal)).toMatch(/^gmu_/);
  expect(asked().map(({ body }) => body)).toEqual([
    { measurements: 5, sessions: 0, uploads: 0 },
    { measurements: 5, sessions: 0, uploads: 0 },
    { measurements: 5, sessions: 0, uploads: 0 },
    { measurements: 1, sessions: 0, uploads: 0 },
    { measurements: 1, sessions: 0, uploads: 1 },
  ]);
});

test("a lease cut below the ask, or a hold cut off on its way, is lowered again, and one ended while taken ends", async () => {
  const { lease, asked } = leased((init) => {
    if (init.method === "DELETE") return { status: 204 };
    const body = JSON.parse(String(init.body));
    if (body.measurements > 32)
      return { body: { token: "gml_t", measurements: 32, sessions: 0, uploads: [] } };
    return body.measurements === 3 ? { afterMs: 100, ...granted(init) } : granted(init);
  });
  const refused = await lease
    .hold({ measurements: 40, sessions: 0 }, new AbortController().signal)
    .catch((error: Error) => error);
  expect(refused instanceof LeaseRefused && refused.reason).toBe("client-limit");
  const abort = new AbortController();
  const cut = lease.hold({ measurements: 2, sessions: 0 }, abort.signal).catch(() => "cut");
  await net!.advance(10);
  abort.abort();
  expect(await cut).toBe("cut");
  await net!.advance();
  lease.end();
  await net!.advance();
  // One being taken when it is ended ends once taken.
  void lease.renew();
  lease.end();
  await net!.advance();
  expect(asked().map(({ method, lease, body }) => [method, lease, body?.measurements])).toEqual([
    ["POST", null, 41],
    ["POST", "gml_t", 1],
    ["POST", "gml_t", 3],
    ["POST", "gml_t", 1],
    ["DELETE", "gml_t", undefined],
    ["POST", null, 1],
    ["DELETE", "gml_t", undefined],
  ]);
});
