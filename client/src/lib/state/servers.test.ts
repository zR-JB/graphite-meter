import "./runes.testutil";
import { expect, test } from "bun:test";
import { stubGlobals } from "../test-helpers.testutil";
import {
  TEST_BUILD_TOKENS,
  testPreparedPaths,
} from "../runner/test-helpers.testutil";
import { singleLatencyBucket } from "../runner/latencyBuckets";
import { LatencyAccumulator } from "../runner/latencySummary";
import { parseCatalog } from "../servers/catalog";
import {
  emptyConnectionValidation,
  type ServerView,
} from "../runner/connectionModel";

const ids = ["constructor", "toString", "__proto__"];

test("valid prototype-named servers retain isolated latency populations through focus and reset", async () => {
  const restore = stubGlobals(TEST_BUILD_TOKENS);
  const { store } = await import("./store.svelte");
  const catalog = parseCatalog(
    {
      servers: [
        { id: "self", name: "Home", url: "." },
        ...ids.map((id, index) => ({
          id,
          name: id,
          url: `https://server-${index}.example`,
        })),
      ],
      defaultSelection: ids,
    },
    "https://home.example",
  );
  const previousCatalog = store.serverCatalog;
  const previousSelection = [...store.selectedServers];
  const previousFocus = store.latencyFocus;
  try {
    store.reset();
    store.serverCatalog = catalog;
    store.selectedServers = ids;
    const summaries = new Map<
      string,
      ReturnType<LatencyAccumulator["snapshot"]>
    >();
    for (const [index, id] of ids.entries()) {
      const stats = new LatencyAccumulator();
      stats.observe((index + 1) * 10, false, 0);
      const summary = stats.snapshot();
      summaries.set(id, summary);
      store.ingest({
        type: "serverLatency",
        serverId: id,
        sample: singleLatencyBucket(100, (index + 1) * 10, false, "download"),
      });
      store.ingest({
        type: "serverLatencySummary",
        serverId: id,
        stage: "download",
        summary,
      });
    }
    for (const [index, id] of ids.entries()) {
      store.focusLatencyServer(id);
      expect(store.latency).toHaveLength(1);
      expect(store.latency[0].medianRttMs).toBe((index + 1) * 10);
      expect(store.latencySummaries.download).toEqual(summaries.get(id));
    }

    // An unfocused server retains later samples; the focused projection updates independently.
    store.focusLatencyServer("constructor");
    store.ingest({
      type: "serverLatency",
      serverId: "toString",
      sample: singleLatencyBucket(200, 25, false, "download"),
    });
    expect(store.latency).toHaveLength(1);
    store.ingest({
      type: "serverLatency",
      serverId: "constructor",
      sample: singleLatencyBucket(200, 15, false, "download"),
    });
    expect(store.latency.map((sample) => sample.medianRttMs)).toEqual([10, 15]);
    store.focusLatencyServer("toString");
    expect(store.latency.map((sample) => sample.medianRttMs)).toEqual([20, 25]);
    const updated = {
      ...summaries.get("toString")!,
      accountingComplete: false,
    };
    store.ingest({
      type: "serverLatencySummary",
      serverId: "toString",
      stage: "download",
      summary: updated,
    });
    expect(store.latencySummaries.download).toEqual(updated);
    expect(store.summariesByServer.get("constructor")?.download).toEqual(
      summaries.get("constructor"),
    );
    expect(store.latencyByServer.get("__proto__")).toHaveLength(1);
    expect(Object.hasOwn(Object.prototype, "download")).toBe(false);

    store.reset();
    expect(store.latencyByServer.size).toBe(0);
    expect(store.summariesByServer.size).toBe(0);
    store.focusLatencyServer("__proto__");
    expect(store.latency).toEqual([]);
    expect(store.latencySummaries).toEqual({});
  } finally {
    store.reset();
    store.serverCatalog = previousCatalog;
    store.selectedServers = previousSelection;
    store.latencyFocus = previousFocus;
    restore();
  }
});

test("selection readiness follows each prototype-named server's view", async () => {
  const restore = stubGlobals(TEST_BUILD_TOKENS);
  const { store } = await import("./store.svelte");
  const previousSelection = [...store.selectedServers];
  const view = (
    id: string,
    readiness: ServerView["readiness"],
  ): ServerView => ({
    server: { id, name: id, url: "https://meter.test" },
    discovery: testPreparedPaths().discovery,
    validation: emptyConnectionValidation(),
    readiness,
    metadataChecking: false,
  });
  try {
    store.selectedServers = ids;
    store.servers.clear();
    expect(store.selectionValidation).toBe("stale");
    for (const id of ids) store.servers.set(id, view(id, "ready"));
    expect(store.selectionValidation).toBe("verified");
    store.servers.set("__proto__", view("__proto__", "checking"));
    expect(store.selectionValidation).toBe("checking");
    store.servers.set("__proto__", view("__proto__", "failed"));
    expect(store.selectionValidation).toBe("failed");
    expect(store.servers.get("constructor")?.readiness).toBe("ready");
  } finally {
    store.servers.clear();
    store.selectedServers = previousSelection;
    restore();
  }
});
