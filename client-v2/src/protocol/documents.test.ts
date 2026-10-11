import { expect, test } from "bun:test";
import { json } from "./pins.testutil";
import { catalog, preflight, probe } from "./documents";

test("the shared preflight and probe fixtures decode as Go and Rust read them", async () => {
  const golden = await json("preflight.golden.json");
  expect(preflight(golden)).toEqual(golden);
  const forward = await json("preflight.forward.golden.json");
  const { throughput, latency } = preflight(forward.document).capabilities;
  expect({ throughput, latency }).toEqual(forward.decoded);
  expect(probe(await json("probe.golden.json"))).toEqual(await json("probe.golden.json"));
});

test("a document with a malformed target, stage limit or text is refused whole", async () => {
  const golden = await json("preflight.golden.json");
  const refused = [
    { ...golden, capabilities: { ...golden.capabilities, maxStageMs: 999 } },
    { ...golden, capabilities: { ...golden.capabilities, latency: [{ baseUrl: ".", transport: "" }] } },
    {
      ...golden,
      capabilities: { ...golden.capabilities, latency: [{ baseUrl: "https://a.example/x", transport: "websocket" }] },
    },
    { ...golden, server: { name: "evil‮txt" } },
    {
      ...golden,
      capabilities: { ...golden.capabilities, throughput: Array(33).fill(golden.capabilities.throughput[0]) },
    },
  ];
  for (const document of refused) expect(() => preflight(document)).toThrow();
});

test("a catalogue binds unique IDs to canonical origins with self first", () => {
  const page = "https://meter.example";
  const parsed = catalog(
    {
      defaultSelection: ["self", "b"],
      servers: [
        { id: "self", url: ".", name: "" },
        { id: "b", url: "https://B.example:443/", name: "B", additionalOrigins: ["https://t.example:8443"] },
        { id: "constructor", url: "http://10.0.0.2:7246", name: "Lab" },
      ],
    },
    page,
  );
  expect(parsed.servers.map(({ id, url, name }) => [id, url, name])).toEqual([
    ["self", page, "self"],
    ["b", "https://b.example", "B"],
    ["constructor", "http://10.0.0.2:7246", "Lab"],
  ]);
  const server = (url: string, id = "x") => ({ id, url, name: "x" });
  for (const servers of [
    [server(".", "b")],
    [server(".", "self"), server(".")],
    [server(".", "self"), server("https://a.example;connect-src:*")],
    [server(".", "self"), server("https://a.example"), server("https://A.example", "y")],
    [server(".", "self"), server("https://a.example:0")],
  ])
    expect(() => catalog({ defaultSelection: ["self"], servers }, page)).toThrow();
  expect(() => catalog({ defaultSelection: ["missing"], servers: [server(".", "self")] }, page)).toThrow();
});
