import { expect, test } from "bun:test";
import {
  allowsServerOrigin,
  parseCatalog,
  reconcileSelection,
  selectedInCatalogOrder,
  validateSelection,
} from "./catalog";

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
