import { createLease } from "./lease";
import { afterEach, expect, test } from "bun:test";
import type { CatalogEntry } from "../../protocol/documents";
import { createBusSlot } from "../lanes/probes";
import type { Discovery, ProbeChannelEvents } from "../types";
import { checkLatency, checkThroughput, discover, failureOf, type CheckContext } from "./checks";
import {
  FakeBuses,
  fakeNet,
  h,
  MARKER,
  page,
  preflight,
  PROBE,
  publicAuth,
  type Answer,
  ws,
  wt,
} from "./fakes.testutil";
import { parseSelection, type Selection } from "./paths";
import { THROTTLED } from "./webtransport";

const entry: CatalogEntry = { id: "self", url: page.origin, name: "Meter", additionalOrigins: [] };
let net: ReturnType<typeof fakeNet> | undefined;
afterEach(() => net?.restore());

/** Discovers a server offering `throughput` and `latency`, then `run`s its check of `choice`; yields the result or row words and when it settled. */
async function check<P>(
  run: (
    context: CheckContext,
    known: Discovery,
    selection: Selection,
    signal: AbortSignal,
  ) => Promise<P>,
  choice: string,
  offer: { throughput: object[]; latency?: object[] },
  probe: (url: URL, attempt: number) => Answer,
  buses = new FakeBuses(),
  browser = page,
) {
  net?.restore();
  let attempt = 0;
  const fake = fakeNet((url) =>
    url.pathname === "/preflight"
      ? { body: preflight(offer.throughput, offer.latency) }
      : probe(url, attempt++),
  );
  net = fake;
  const auth = publicAuth(entry);
  const context: CheckContext = {
    entry,
    auth,
    lease: createLease(entry.url, auth),
    page: browser,
    bus: createBusSlot(buses.open, {} as ProbeChannelEvents),
    now: fake.now,
  };
  const signal = new AbortController().signal;
  const known = await discover(context, signal);
  const outcome = run(context, known, parseSelection(choice), signal).then(
    (value) => ({ at: fake.now(), value }),
    (error: unknown) => ({ at: fake.now(), failure: failureOf(error, entry.name) }),
  );
  await fake.advance(10_000);
  return outcome;
}

test("HTTP/3 is probed until the browser uses it; past its 2 s bound the last answer says what it used", async () => {
  const offer = { throughput: [h(".", "http3")] };
  for (const [hops, at, outcome] of [
    [["h2", "h2", "h3"], 1_050, "h3"],
    [["h2"], 2_000, "The browser used HTTP/2, not HTTP/3"],
    [[null], 2_000, "The browser did not confirm HTTP/3"],
    [[], 2_000, "Connection check timed out"],
  ] as const) {
    const result = await check(checkThroughput, "protocol:http3", offer, (_, attempt) =>
      hops.length
        ? { body: PROBE, afterMs: 300, hop: hops[Math.min(attempt, hops.length - 1)] }
        : "hang",
    );
    const shown = "value" in result ? result.value.browserProtocol : result.failure.message;
    expect({ at: result.at, shown }, hops.join()).toEqual({ at, shown: outcome });
  }
});

test("Automatic gives each candidate its own budget, names why it fell back, and stops at a sign-in refusal", async () => {
  const offer = { throughput: [h(".", "http1"), h("https://meter.example:8443", "http2")] };
  // An HTTP/1.1 head that never answers cannot hold Automatic past its budget.
  const fallback = await check(checkThroughput, "auto", offer, (url) =>
    url.port ? { body: PROBE, hop: "h2" } : "hang",
  );
  expect(
    "value" in fallback && {
      at: fallback.at,
      ...fallback.value.verifiedFor,
      id: fallback.value.target.id,
    },
  ).toEqual({
    at: 2_500,
    selection: "auto",
    head: page.origin,
    fallback: "Connection check timed out",
    id: "https://meter.example:8443",
  });
  const refused = await check(checkThroughput, "auto", offer, () => ({
    status: 403,
    headers: MARKER,
  }));
  expect(refused).toEqual({
    at: 0,
    failure: { message: "Sign in to Meter", retry: "needs-user", signIn: true },
  });
  expect(net?.requests.map(({ url }) => url.origin + url.pathname)).toEqual([
    `${page.origin}/preflight`,
    `${page.origin}/probe`,
  ]);
});

test("a latency path needs its probe and a bus answering within 3.5 s; five replies end its check, and the bus stays", async () => {
  const offer = { throughput: [], latency: [ws(".")] };
  const buses = new FakeBuses();
  const replies = await check(checkLatency, "auto", offer, () => ({ body: PROBE }), buses);
  expect("value" in replies && { at: replies.at, rttMs: replies.value.rttMs }).toEqual({
    at: 490,
    rttMs: 10,
  });
  expect(buses.opened.map(({ disposed, intervalMs }) => [disposed, intervalMs])).toEqual([
    [false, 1_000],
  ]);
  const silent = new FakeBuses();
  silent.rttMs = null;
  const quiet = await check(checkLatency, "auto", offer, () => ({ body: PROBE }), silent);
  expect(quiet).toEqual({
    at: 3_500,
    failure: { message: "Latency channel did not become ready", retry: "retryable", signIn: false },
  });
  expect(silent.opened.every(({ disposed }) => disposed)).toBe(true);
});

test("a throttled page's Automatic latency uses WebSocket and says why; a chosen WebTransport waits for the user", async () => {
  const offer = {
    throughput: [h(".", "http1")],
    latency: [wt("https://meter.example:8443"), ws(".")],
  };
  const throttled = { ...page, webTransport: "throttled" as const };
  const buses = new FakeBuses();
  const automatic = await check(
    checkLatency,
    "auto",
    offer,
    () => ({ body: PROBE }),
    buses,
    throttled,
  );
  expect(
    "value" in automatic && {
      transport: automatic.value.target.transport,
      fallback: automatic.value.verifiedFor.fallback,
    },
  ).toEqual({ transport: "websocket", fallback: THROTTLED });
  const chosen = await check(
    checkLatency,
    "transport:webtransport",
    offer,
    () => ({ body: PROBE }),
    buses,
    throttled,
  );
  expect(chosen).toEqual({
    at: 0,
    failure: { message: THROTTLED, retry: "needs-user", signIn: false },
  });
  expect(buses.opened.map(({ options }) => options.target.transport)).toEqual(["websocket"]);
});

test("a transfer port whose probe names another protocol is named and waits for the user", async () => {
  const other = "https://meter.example:8443";
  // Whichever candidate another release serves, the other's passing trouble does not hide it.
  for (const newer of [entry.url, other]) {
    const outcome = await check(
      checkThroughput,
      "auto",
      { throughput: [h(".", "http1"), h(other, "http1")] },
      (url) =>
        url.origin === newer
          ? { body: { ...PROBE, engineVersion: "0.15.0", protocol: 3 }, hop: "http/1.1" }
          : { status: 503 },
    );
    expect(outcome, newer).toMatchObject({
      failure: {
        message: `${newer} runs Graphite Meter 0.15.0, newer than this page (9.9.9); reload the page or update the server this page comes from.`,
        retry: "needs-user",
      },
    });
  }
});
