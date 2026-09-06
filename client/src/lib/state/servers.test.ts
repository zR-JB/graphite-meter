import "./runes.test";
import { expect, test } from "bun:test";
import { stubGlobals } from "../test-helpers.test";
import {
  TEST_BUILD_TOKENS,
  testPreparedPaths,
} from "../runner/test-helpers.test";
import { singleLatencyBucket } from "../runner/latencyBuckets";
import { LatencyAccumulator } from "../runner/latencySummary";
import { parseCatalog } from "../servers/catalog";

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

test("readiness and discovery updates keep prototype-named server identities separate", async () => {
  const restore = stubGlobals(TEST_BUILD_TOKENS);
  const { store } = await import("./store.svelte");
  const previousSelection = [...store.selectedServers];
  try {
    store.selectedServers = ids;
    store.serverReadiness.clear();
    store.serverDiscoveries.clear();
    expect(store.selectionValidation).toBe("stale");
    for (const [index, id] of ids.entries()) {
      store.serverReadiness.set(id, { state: "ready" });
      store.serverDiscoveries.set(id, {
        ...testPreparedPaths().discovery,
        server: { name: `Server ${index}` },
      });
    }
    expect(store.selectionValidation).toBe("verified");
    store.serverReadiness.set("__proto__", { state: "checking" });
    expect(store.selectionValidation).toBe("checking");
    store.serverReadiness.set("__proto__", { state: "failed" });
    expect(store.selectionValidation).toBe("failed");
    expect(store.serverReadiness.get("constructor")?.state).toBe("ready");
    expect(store.serverDiscoveries.get("__proto__")?.server.name).toBe(
      "Server 2",
    );
    store.serverDiscoveries.delete("toString");
    expect(store.serverDiscoveries.has("toString")).toBe(false);
    expect(store.serverDiscoveries.get("constructor")?.server.name).toBe(
      "Server 0",
    );
    store.serverReadiness.clear();
    expect(store.selectionValidation).toBe("stale");
  } finally {
    store.serverReadiness.clear();
    store.serverDiscoveries.clear();
    store.selectedServers = previousSelection;
    restore();
  }
});
