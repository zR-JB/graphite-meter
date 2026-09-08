import { expect, test } from "bun:test";
import {
  allowsServerOrigin,
  browserOriginRestriction,
  parseCatalog,
  reconcileSelection,
  selectedInCatalogOrder,
  validateSelection,
} from "./catalog";

test("browser IPv6 origins require DNS except for the interface's exact origin", () => {
  const ui = "http://[::1]:7246";
  expect(browserOriginRestriction(ui, ui)).toBeUndefined();
  expect(
    browserOriginRestriction("https://meter.example:7248", ui),
  ).toBeUndefined();
  for (const [origin, page] of [
    ["http://[::1]:7247", ui],
    ["https://[::1]:7246", ui],
    ["http://[::2]:7246", ui],
    ["https://[2001:db8::1]", "https://meter.example"],
  ])
    expect(browserOriginRestriction(origin, page)).toContain("DNS hostname");
});

test.each([
  {
    label: "public clear server from HTTPS",
    origin: "http://meter.example:7246",
    page: "https://ui.example",
    restriction:
      "Use an HTTPS origin for this server when the interface is HTTPS.",
  },
  {
    label: "private clear server from HTTPS",
    origin: "http://192.168.1.20:7246",
    page: "https://ui.example",
    restriction:
      "Use an HTTPS origin for this server when the interface is HTTPS.",
  },
  {
    label: "localhost clear server from HTTPS",
    origin: "http://localhost:7246",
    page: "https://ui.example",
    restriction: undefined,
  },
  {
    label: "localhost subdomain clear server from HTTPS",
    origin: "http://meter.localhost:7246",
    page: "https://ui.example",
    restriction: undefined,
  },
  {
    label: "IPv4 loopback clear server from HTTPS",
    origin: "http://127.255.1.2:7246",
    page: "https://ui.example",
    restriction: undefined,
  },
  {
    label: "clear server from HTTP",
    origin: "http://meter.example:7246",
    page: "http://ui.example",
    restriction: undefined,
  },
  {
    label: "HTTPS server from HTTP",
    origin: "https://meter.example:7248",
    page: "http://ui.example",
    restriction: undefined,
  },
] as const)(
  "browser origin policy: $label",
  ({ origin, page, restriction }) => {
    expect(browserOriginRestriction(origin, page)).toBe(restriction);
  },
);

const catalog = parseCatalog(
  {
    defaultSelection: ["b"],
    servers: [
      { id: "self", url: ".", name: "Home" },
      { id: "a", url: "https://a.example", name: "A" },
      { id: "b", url: "https://b.example:443/", name: "B" },
      { id: "c", url: "http://c.example", name: "C" },
      { id: "d", url: "https://d.example", name: "D" },
    ],
  },
  "https://home.example",
);
test("operator defaults, saved overrides, and deselecting self retain catalogue order", () => {
  expect(reconcileSelection(catalog, null)).toEqual({
    ids: ["b"],
    unresolved: [],
  });
  const saved = [
    { id: "b", url: "https://b.example" },
    { id: "a", url: "https://a.example" },
  ];
  expect(
    selectedInCatalogOrder(catalog, reconcileSelection(catalog, saved).ids).map(
      (server) => server.id,
    ),
  ).toEqual(["a", "b"]);
});
test("a changed or removed identity requires explicit reconciliation", () => {
  expect(
    reconcileSelection(catalog, [
      { id: "a", url: "https://old.example" },
      { id: "removed", url: "https://removed.example" },
    ]),
  ).toEqual({
    ids: [],
    unresolved: [
      { id: "a", url: "https://old.example" },
      { id: "removed", url: "https://removed.example" },
    ],
  });
  expect(() => validateSelection(catalog, [])).toThrow();
  expect(() =>
    validateSelection(catalog, ["self", "a", "b", "c", "d"]),
  ).toThrow();
});
test("transport ports stay within the selected deployment's named origins", () => {
  const server = {
    ...catalog.servers[1],
    additionalOrigins: ["https://bulk.example:9443"],
  };
  expect(allowsServerOrigin(server, "https://a.example:8443")).toBe(true);
  expect(allowsServerOrigin(server, "https://bulk.example:9443")).toBe(true);
  for (const origin of [
    "https://bulk.example",
    "https://evil.a.example",
    "https://a.example.attacker.test",
    "https://a.example;connect-src:*",
  ])
    expect(allowsServerOrigin(server, origin)).toBe(false);
});
test("catalogues reject ambiguous IDs, duplicate origins, and oversized populations", () => {
  for (const servers of [
    [...catalog.servers, { id: "bad!", url: "https://x.example", name: "X" }],
    [
      ...catalog.servers,
      { id: "duplicate", url: "https://a.example", name: "X" },
    ],
    Array.from({ length: 33 }, (_, i) => ({
      id: i === 0 ? "self" : `s${i}`,
      url: `https://s${i}.example`,
      name: "Server",
    })),
  ])
    expect(() =>
      parseCatalog(
        { defaultSelection: ["self"], servers },
        "https://home.example",
      ),
    ).toThrow();
});
