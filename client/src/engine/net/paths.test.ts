import { createLease } from "./lease";
import { expect, test } from "bun:test";
import type { Preflight } from "../../protocol/documents";
import { createBusSlot } from "../lanes/probes";
import type { Discovery, ProbeChannelEvents } from "../types";
import { discover } from "./checks";
import { FakeBuses, fakeNet, h, page, publicAuth, ws, wt } from "./fakes.testutil";
import {
  candidates,
  classify,
  originPolicy,
  parseSelection,
  portable,
  resolve,
  selectionText,
  stillValid,
  unresolvedReason,
} from "./paths";

const MIXED = "Use an HTTPS origin for this server when the interface is HTTPS.";
const IPV6 = "Use a DNS hostname for browser connections to this IPv6 server.";

function offer(
  throughput: object[],
  latency: object[] = [],
  {
    origin = page.origin,
    protocol = null as string | null,
    generation = "g1",
    browser = page,
  } = {},
): Discovery {
  const capabilities = { throughput, latency } as Preflight["capabilities"];
  return {
    generation,
    engineVersion: "",
    server: { name: "" },
    maxStageMs: 300_000,
    origin,
    protocol,
    readAt: 0,
    requestMs: 0,
    ...classify(capabilities, origin, browser),
  };
}

test("this page's policy refuses origins it cannot reach, before any request", async () => {
  for (const [origin, pageOrigin, reason] of [
    ["http://peer.example:7246", "https://meter.example", MIXED],
    ["http://128.0.0.1:7246", "https://meter.example", MIXED],
    ["http://localhost:7246", "https://meter.example", null],
    ["http://speed.localhost", "https://meter.example", null],
    ["http://127.0.0.9:7246", "https://meter.example", null],
    ["https://[2001:db8::1]:7247", "https://meter.example", IPV6],
    ["http://[::1]:7246", "https://meter.example", IPV6],
    ["https://[2001:db8::1]:7247", "https://[2001:db8::1]:7247", null],
    ["http://peer.example:7246", "http://meter.example", null],
    ["https://peer.example", "http://meter.example", null],
  ] as const) {
    const browser = { origin: pageOrigin, secure: pageOrigin.startsWith("https:") };
    expect(originPolicy(origin, browser), `${origin} from ${pageOrigin}`).toBe(reason);
  }
  const mixed = offer([
    h("http://peer.example:7246", "http1"),
    h("http://localhost:7246", "http1"),
  ]);
  expect([...mixed.throughput].map(([origin, { blocked }]) => [origin, blocked])).toEqual([
    ["http://peer.example:7246", MIXED],
    ["http://localhost:7246", null],
  ]);
  expect(
    unresolvedReason(mixed, "throughput", parseSelection("http://peer.example:7246"), page),
  ).toEqual({
    message: MIXED,
    retry: "impossible",
  });
  const net = fakeNet(() => "unreachable");
  try {
    for (const [url, message] of [
      ["http://peer.example:7246", MIXED],
      ["https://[2001:db8::1]:7247", IPV6],
    ] as const) {
      const entry = { id: "peer", url, name: "Peer", additionalOrigins: [] };
      const context = {
        entry,
        auth: publicAuth(entry),
        lease: createLease(url, publicAuth(entry)),
        page,
        bus: createBusSlot(new FakeBuses().open, {} as ProbeChannelEvents),
        now: net.now,
      };
      await expect(discover(context, new AbortController().signal)).rejects.toMatchObject({
        message,
        retry: "impossible",
      });
    }
    expect(net.requests).toEqual([]);
  } finally {
    net.restore();
  }
});

test("WebTransport needs an https origin and a page that has the API", () => {
  const advertised = offer(
    [
      h("https://meter.example", "http1"),
      wt("https://meter.example:7249"),
      wt("http://meter.example:7246"),
    ],
    [ws("https://meter.example"), wt("https://meter.example:7249")],
  );
  expect([...advertised.throughput.keys()]).toEqual([
    "https://meter.example",
    "https://meter.example:7249",
  ]);
  const choice = parseSelection("transport:webtransport");
  for (const [webTransport, message] of [
    ["insecure-page", "WebTransport needs a secure page; open this interface over HTTPS."],
    ["no-api", "This browser does not support WebTransport."],
  ] as const) {
    const browser = { ...page, webTransport };
    expect(resolve(advertised, "throughput", choice, browser)).toBeNull();
    expect(unresolvedReason(advertised, "throughput", choice, browser)).toEqual({
      message,
      retry: "impossible",
    });
    expect(candidates(advertised, "latency", browser).map(({ id }) => id)).toEqual([
      "https://meter.example",
    ]);
  }
});

test("Automatic ranks by mechanism, then origin, never by the order of advertisement", () => {
  const offered = [
    h("https://meter.example", "http1"),
    h("https://meter.example:2", "http2"),
    h("https://meter.example:3", "http3"),
    wt("https://meter.example:3"),
    wt("https://meter.example:3", "webtransport-datagram"),
  ];
  const ranked = [
    "https://meter.example",
    "https://meter.example:2",
    "https://meter.example:3",
    "https://meter.example:3::wt",
  ];
  for (const order of [offered, offered.toReversed()])
    expect(candidates(offer(order), "throughput", page).map(({ id }) => id)).toEqual(ranked);
  const latency = offer([], [ws("https://meter.example"), wt("https://meter.example:3")]);
  expect(candidates(latency, "latency", page).map(({ id }) => id)).toEqual([
    "https://meter.example:3::wt",
    "https://meter.example",
  ]);
  // A proxy's negotiated "." ranks by the protocol its preflight used. A tie goes to the discovery origin.
  const proxy = [h(".", "negotiated"), h("https://proxy.example:8443", "http3")];
  for (const [protocol, first] of [
    ["h2", "https://proxy.example"],
    ["h3", "https://proxy.example"],
    [null, "https://proxy.example:8443"],
  ] as const)
    expect(
      resolve(
        offer(proxy, [], { origin: "https://proxy.example", protocol }),
        "throughput",
        { kind: "auto" },
        page,
      )?.id,
    ).toBe(first);
  // A target naming its protocol replaces a negotiated one of the same origin, in either order.
  for (const order of [
    [h("https://meter.example", "http1"), h(".", "negotiated")],
    [h(".", "negotiated"), h("https://meter.example", "http1")],
  ])
    expect(
      offer(order)
        .throughput.get("https://meter.example")
        ?.targets.map(({ protocol }) => protocol),
    ).toEqual(["http1"]);
  // WebTransport streams are Automatic's last resort.
  const last = offer([wt("https://wt.example"), h("https://meter.example", "http3")]);
  expect(candidates(last, "throughput", page).map(({ id }) => id)).toEqual([
    "https://meter.example",
    "https://wt.example::wt",
  ]);
});

test("an explicit choice resolves to its own mechanism or to nothing", () => {
  const advertised = offer(
    [
      h("https://meter.example", "http1"),
      h("https://meter.example:8443", "http2"),
      wt("https://meter.example:8443"),
    ],
    [ws("https://meter.example"), wt("https://meter.example"), wt("https://wt.example")],
  );
  const noApi = { ...page, webTransport: "no-api" } as const;
  for (const [role, choice, id, browser = page] of [
    ["latency", "https://meter.example", "https://meter.example"],
    ["latency", "https://meter.example::wt", "https://meter.example::wt"],
    ["latency", "transport:websocket", "https://meter.example"],
    ["latency", "https://wt.example", "https://wt.example::wt"],
    ["latency", "https://wt.example", null, noApi],
    ["throughput", "protocol:http2", "https://meter.example:8443"],
    ["throughput", "https://meter.example:8443", "https://meter.example:8443"],
    ["throughput", "https://meter.example:8443::wt", "https://meter.example:8443::wt"],
    ["throughput", "protocol:http3", null],
    ["throughput", "https://other.example", null],
  ] as const)
    expect(
      resolve(advertised, role, parseSelection(choice), browser)?.id ?? null,
      `${role} ${choice}`,
    ).toBe(id);
  for (const [choice, message] of [
    ["protocol:http3", "HTTP/3 not offered"],
    ["transport:webtransport-datagram", "WebTransport datagrams not offered"],
    ["https://other.example", "https://other.example not offered"],
  ] as const)
    expect(unresolvedReason(advertised, "throughput", parseSelection(choice), page)).toEqual({
      message,
      retry: "needs-user",
    });
});

test("a verified path stays valid while its choice resolves as when it was verified", () => {
  const before = offer([
    h("https://meter.example", "http1"),
    h("https://meter.example:8443", "http2"),
  ]);
  // Automatic verified its fallback while the HTTP/1.1 head failed.
  const fallback = {
    generation: "g1",
    verifiedFor: { selection: "auto", head: "https://meter.example" },
  };
  const auto = parseSelection("auto");
  expect(stillValid(fallback, before, "throughput", auto, page)).toBe(true);
  expect(
    stillValid(
      fallback,
      offer([h("https://meter.example:8443", "http2")]),
      "throughput",
      auto,
      page,
    ),
  ).toBe(false);
  expect(stillValid(fallback, { ...before, generation: "g2" }, "throughput", auto, page)).toBe(
    false,
  );
  expect(stillValid(fallback, before, "throughput", parseSelection("protocol:http1"), page)).toBe(
    false,
  );
});

test("a portable choice keeps the mechanism and drops the origin", () => {
  const advertised = offer(
    [
      h(".", "negotiated"),
      h("https://meter.example:8443", "http2"),
      wt("https://meter.example:8443"),
    ],
    [ws("https://meter.example")],
  );
  for (const [role, choice, travels, discovery = advertised] of [
    ["throughput", "auto", "auto"],
    ["throughput", "protocol:http2", "protocol:http2"],
    ["throughput", "https://meter.example:8443", "protocol:http2"],
    ["throughput", "https://meter.example", "auto"],
    ["throughput", "https://meter.example:8443::wt", "transport:webtransport"],
    ["throughput", "https://gone.example::wt", "transport:webtransport"],
    ["throughput", "https://gone.example::wtdg", "transport:webtransport-datagram"],
    ["latency", "https://gone.example::wtdg", "auto"],
    ["latency", "https://meter.example", "transport:websocket"],
    ["throughput", "https://meter.example:8443", "auto", null],
  ] as const)
    expect(
      selectionText(portable(role, parseSelection(choice), discovery, page)),
      `${role} ${choice}`,
    ).toBe(travels);
  for (const text of [
    "auto",
    "protocol:http1",
    "transport:webtransport-datagram",
    "https://a.example::wt",
    "protocol:http4",
  ])
    expect(selectionText(parseSelection(text))).toBe(text);
});
