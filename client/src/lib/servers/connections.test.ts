import { beforeEach, afterEach, expect, spyOn, test } from "bun:test";
import { DEFAULT_CONFIG } from "../state/defaults";
import { testPreparedPaths } from "../runner/test-helpers.test";
import type { ConnectionPreparation } from "../runner/real/prepare";
import { ServerConnections, type ServerConnectionView } from "./connections";
import { CONNECTION_FRESH_MS } from "../runner/connectionModel";
import { PreflightUnavailableError } from "../runner/real/transportError";
import { ServerAuthenticationRequired } from "./credentials";

import { stubGlobals } from "../test-helpers.test";
let restore: () => void;
beforeEach(() => {
  restore = stubGlobals({ location: new URL("http://meter.test") });
});
afterEach(() => restore());

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
async function settle() {
  for (let i = 0; i < 40; i++) await Promise.resolve();
}
function preparation(): ConnectionPreparation {
  const paths = testPreparedPaths();
  return {
    discovery: paths.discovery,
    validation: {
      throughput: {
        selection: "auto",
        state: "verified",
        path: paths.throughput,
      },
      latency: { selection: "auto", state: "verified", path: paths.latency },
    },
  };
}
function fixture(
  prepare: ConstructorParameters<typeof ServerConnections>[0]["prepare"],
  discover: ConstructorParameters<
    typeof ServerConnections
  >[0]["discover"] = async () => testPreparedPaths().discovery,
) {
  const views = new Map<string, ServerConnectionView>();
  const manager = new ServerConnections({
    discover,
    prepare,
    changed: (view) => {
      views.set(view.server.id, view);
    },
    idleEvent() {},
  });
  manager.reset(
    ["self", "peer"].map((id) => ({ id, name: id, url: "http://meter.test" })),
  );
  const config = structuredClone(DEFAULT_CONFIG);
  manager.select([
    { id: "self", config },
    { id: "peer", config },
  ]);
  return { manager, views, config };
}

test("a failed role preserves the independently verified role", async () => {
  const { manager, views } = fixture(async (_config, _previous, roles) => {
    if (roles.includes("latency")) throw new Error("offline");
    return preparation();
  });
  try {
    await expect(manager.check({ ids: ["self"] })).rejects.toThrow();
    expect(views.get("self")!.validation.throughput.state).toBe("verified");
    expect(views.get("self")!.validation.latency.state).toBe("failed");
    expect(manager.paths("self")).toBeNull();
  } finally {
    manager.dispose();
  }
});

test("wrapped remote authentication retains actionable sign-in details", async () => {
  const { manager, views } = fixture(
    async () => preparation(),
    async (_signal, context) => {
      throw new PreflightUnavailableError("preflight unavailable", {
        cause: new ServerAuthenticationRequired(context!.server),
      });
    },
  );
  try {
    await expect(manager.check({ ids: ["peer"] })).rejects.toThrow(
      "Sign in to peer",
    );
    expect(views.get("peer")!.readiness).toMatchObject({
      state: "sign-in",
      message: "Sign in to peer",
    });
  } finally {
    manager.dispose();
  }
});

test("opaque HTTPS failure from an HTTP interface offers conditional guidance without inventing authentication", async () => {
  const { manager, views, config } = fixture(
    async () => preparation(),
    async () => {
      throw new PreflightUnavailableError("preflight unavailable", {
        cause: new TypeError("Failed to fetch"),
      });
    },
  );
  try {
    manager.reset([{ id: "peer", name: "Peer", url: "https://peer.example" }]);
    manager.select([{ id: "peer", config }]);
    await expect(manager.check()).rejects.toThrow(
      "If it requires sign-in, open this interface over HTTPS",
    );
    expect(views.get("peer")!.readiness.state).toBe("failed");
  } finally {
    manager.dispose();
  }
});

test("fresh equivalent selections reuse evidence, but expired reselections refresh discovery and both roles", async () => {
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  const checked: string[] = [];
  const refreshed = deferred<void>();
  let discoveries = 0;
  const { manager, views, config } = fixture(
    async (_config, _previous, roles) => {
      checked.push(...roles);
      if (checked.length === 4) refreshed.resolve();
      return preparation();
    },
    async () => {
      discoveries++;
      return testPreparedPaths().discovery;
    },
  );
  try {
    manager.select([{ id: "self", config }]);
    await manager.check();
    const equivalent = structuredClone(config);
    equivalent.transports.throughputTarget =
      preparation().validation.throughput.path!.target.origin;
    equivalent.transports.latencyTarget =
      preparation().validation.latency.path!.target.origin;
    manager.select([{ id: "self", config: equivalent }]);
    await manager.check();
    expect(checked).toEqual(["throughput", "latency"]);
    expect(manager.ready()).toBe(true);
    manager.select([]);
    clock.mockReturnValue(1000 + CONNECTION_FRESH_MS + 1);
    manager.select([{ id: "self", config: equivalent }]);
    expect(manager.ready()).toBe(false);
    expect(views.get("self")!.readiness.state).toBe("unchecked");
    manager.activity(true, null);
    await refreshed.promise;
    await settle();
    expect(discoveries).toBe(2);
    expect(checked).toEqual(["throughput", "latency", "throughput", "latency"]);
    expect(manager.ready()).toBe(true);
  } finally {
    manager.dispose();
    clock.mockRestore();
  }
});

test("refreshing expired discovery retains a newer verified latency monitor for the same generation", async () => {
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  const monitors: { active: boolean }[] = [];
  let discoveries = 0;
  let probes = 0;
  const { manager, config } = fixture(
    async (_config, _previous, roles) => {
      probes++;
      const result = preparation();
      if (roles.includes("latency")) {
        const monitor = { active: false };
        monitors.push(monitor);
        result.idle = {
          start() {
            monitor.active = true;
          },
          stop() {
            monitor.active = false;
          },
          onEvent() {},
        };
      }
      return result;
    },
    async () => {
      discoveries++;
      return testPreparedPaths().discovery;
    },
  );
  try {
    manager.select([{ id: "self", config }]);
    await manager.check();
    manager.activity(true, "self");
    clock.mockReturnValue(91000);
    manager.invalidate();
    await manager.check();
    const monitor = monitors.at(-1)!;
    expect(monitor.active).toBe(true);
    expect(discoveries).toBe(1);
    clock.mockReturnValue(122000);
    manager.select([{ id: "self", config }]);
    await manager.check();
    expect(discoveries).toBe(2);
    expect(probes).toBe(4);
    expect(manager.ready()).toBe(true);
    expect(monitor.active).toBe(true);
  } finally {
    manager.dispose();
    clock.mockRestore();
  }
});

test("a generation change cancels an in-flight role before accepting replacement evidence", async () => {
  const held = deferred<ConnectionPreparation>();
  let generation = "gen-a";
  let block = false;
  let oldSignal: AbortSignal | undefined;
  const { manager, views } = fixture(
    async (_config, _previous, roles, signal) => {
      if (block && roles.includes("latency")) {
        oldSignal = signal;
        return held.promise;
      }
      const result = preparation();
      result.validation.throughput.path!.generation = generation;
      result.validation.latency.path!.generation = generation;
      return result;
    },
    async () => ({ ...testPreparedPaths().discovery, generation }),
  );
  try {
    await manager.check({ ids: ["self"] });
    block = true;
    const old = manager
      .check({ ids: ["self"], role: "latency", force: true })
      .catch((error) => error);
    await settle();
    expect(oldSignal?.aborted).toBe(false);
    generation = "gen-b";
    block = false;
    await manager.check({ ids: ["self"], role: "throughput", force: true });
    expect((await old).name).toBe("AbortError");
    expect(oldSignal?.aborted).toBe(true);
    expect(manager.ready(["self"])).toBe(true);
    held.resolve(preparation());
    await settle();
    expect(views.get("self")!.validation.latency.path!.generation).toBe(
      "gen-b",
    );
  } finally {
    manager.dispose();
  }
});

test("changing the latency participant cancels its probe and cannot adopt late evidence when re-enabled", async () => {
  const held = deferred<ConnectionPreparation>();
  let latencyChecks = 0;
  let stopped = 0;
  const { manager, views, config } = fixture(
    async (_config, _previous, roles) => {
      if (roles.includes("latency") && ++latencyChecks === 1)
        return held.promise;
      return preparation();
    },
  );
  try {
    manager.select([{ id: "self", config }]);
    const old = manager.check().catch((error) => error);
    await settle();
    expect(views.get("self")!.validation.throughput.state).toBe("verified");
    const noLatency = structuredClone(config);
    noLatency.stages.latency = false;
    noLatency.skipLoadedLatencyWhenStageOff = true;
    manager.select([{ id: "self", config: noLatency }]);
    expect((await old).name).toBe("AbortError");
    expect(manager.ready()).toBe(true);
    manager.select([{ id: "self", config }]);
    expect(manager.ready()).toBe(false);
    await manager.check();
    const current = views.get("self")!.validation.latency.path;
    held.resolve({
      ...preparation(),
      idle: {
        start() {},
        stop() {
          stopped++;
        },
        onEvent() {},
      },
    });
    await settle();
    expect(latencyChecks).toBe(2);
    expect(stopped).toBe(1);
    expect(views.get("self")!.validation.latency.path).toBe(current);
    expect(manager.ready()).toBe(true);
  } finally {
    manager.dispose();
  }
});

test("removing a blocked server releases capacity and rejects its late evidence", async () => {
  const held = deferred<ConnectionPreparation>();
  let stopped = 0;
  const { manager, views, config } = fixture(
    async (_config, _previous, _roles, _signal, credentials) => {
      return credentials?.server.id === "self" ? held.promise : preparation();
    },
  );
  try {
    const old = manager.check({ ids: ["self"] }).catch((error) => error);
    await settle();
    expect(views.get("self")!.readiness.state).toBe("checking");
    manager.select([{ id: "peer", config }]);
    await manager.check({ ids: ["peer"] });
    expect(manager.ready(["peer"])).toBe(true);
    expect((await old).name).toBe("AbortError");
    held.resolve({
      ...preparation(),
      idle: {
        start() {},
        stop() {
          stopped++;
        },
        onEvent() {},
      },
    });
    await settle();
    expect(stopped).toBe(2);
    expect(views.get("self")!.validation.throughput.state).toBe("stale");
  } finally {
    manager.dispose();
  }
});

test("cancelling Start aborts its probes without blocking a later check", async () => {
  const held = deferred<ConnectionPreparation>();
  const signals: AbortSignal[] = [];
  let block = true;
  const { manager } = fixture(async (_config, _previous, _roles, signal) => {
    signals.push(signal!);
    return block ? held.promise : preparation();
  });
  try {
    const start = new AbortController();
    const pending = manager
      .check({ ids: ["self"], signal: start.signal })
      .catch((error) => error);
    await settle();
    expect(signals.length).toBe(2);
    start.abort();
    expect((await pending).name).toBe("AbortError");
    expect(signals.every((signal) => signal.aborted)).toBe(true);
    block = false;
    await settle();
    await manager.check({ ids: ["self"] });
    expect(manager.ready(["self"])).toBe(true);
    held.resolve(preparation());
  } finally {
    manager.dispose();
  }
});

test("a probe deadline releases capacity even when the network adapter ignores cancellation", async () => {
  const held = deferred<ConnectionPreparation>();
  const originalTimeout = globalThis.setTimeout;
  const timer = spyOn(globalThis, "setTimeout").mockImplementation(((
    ...[run, delay, ...args]: Parameters<typeof setTimeout>
  ) =>
    originalTimeout(
      run,
      delay === 12000 ? 0 : delay,
      ...args,
    )) as typeof setTimeout);
  const { manager, views } = fixture(
    async (_config, _previous, _roles, _signal, credentials) =>
      credentials?.server.id === "self" ? held.promise : preparation(),
  );
  try {
    const failed = manager.check({ ids: ["self"] }).catch((error) => error);
    await settle();
    await manager.check({ ids: ["peer"] });
    expect((await failed).message).toBe("Connection check timed out");
    expect(manager.ready(["peer"])).toBe(true);
    expect(views.get("self")!.readiness.state).toBe("failed");
    held.resolve(preparation());
    await settle();
    expect(manager.paths("self")).toBeNull();
  } finally {
    manager.dispose();
    timer.mockRestore();
  }
});

test("renewed authorization discards old work and resumes only the affected server", async () => {
  const held = deferred<ConnectionPreparation>();
  let block = true;
  const checked: string[] = [];
  const { manager, views } = fixture(
    async (_config, _previous, _roles, _signal, credentials) => {
      checked.push(credentials!.server.id);
      return block ? held.promise : preparation();
    },
  );
  try {
    const old = manager.check({ ids: ["self"] }).catch((error) => error);
    await settle();
    manager.requireAuthentication("self", "Sign in again");
    expect(views.get("self")!.readiness.state).toBe("sign-in");
    expect((await old).name).toBe("AbortError");
    const credentials = manager.credentials("self")!;
    manager.authorize({
      ...credentials,
      kind: "grant",
      token: "test-only",
      expiresAt: Date.now() + 60000,
    });
    block = false;
    await manager.check({ ids: ["self"] });
    expect(manager.ready(["self"])).toBe(true);
    expect(checked.every((id) => id === "self")).toBe(true);
    held.resolve(preparation());
    await settle();
    expect(views.get("self")!.readiness.state).toBe("ready");
  } finally {
    manager.dispose();
  }
});

for (const failure of ["discovery", "latency"] as const)
  test(`home recovery preserves peer ${failure} backoff and manual retry while global recovery retries it`, async () => {
    const clock = spyOn(Date, "now").mockReturnValue(1000);
    const originalTimeout = globalThis.setTimeout;
    const delays: number[] = [];
    const timer = spyOn(globalThis, "setTimeout").mockImplementation(((
      ...[run, delay, ...args]: Parameters<typeof setTimeout>
    ) => {
      delays.push(Number(delay ?? 0));
      return originalTimeout(run, delay, ...args);
    }) as typeof setTimeout);
    let attempts = 0;
    const { manager, views } = fixture(
      async (_config, _previous, roles, _signal, credentials) => {
        if (
          failure === "latency" &&
          credentials?.server.id === "peer" &&
          roles.includes("latency")
        ) {
          attempts++;
          throw new Error("Peer unavailable");
        }
        return preparation();
      },
      async (_signal, credentials) => {
        if (failure === "discovery" && credentials?.server.id === "peer") {
          attempts++;
          throw new Error("Peer unavailable");
        }
        return testPreparedPaths().discovery;
      },
    );
    try {
      await manager.check({ ids: ["self"] });
      await expect(manager.check({ ids: ["peer"] })).rejects.toThrow();
      manager.activity(true, null);
      manager.recover("self");
      expect(delays.at(-1)).toBe(30000);
      await new Promise((resolve) => originalTimeout(resolve, 0));
      await settle();
      expect(attempts).toBe(1);
      expect(views.get("peer")!.readiness.state).toBe("failed");

      await expect(
        manager.check({ ids: ["peer"], force: true }),
      ).rejects.toThrow();
      expect(attempts).toBe(2);
      manager.recover("self");
      expect(delays.at(-1)).toBe(60000);
      expect(views.get("peer")!.readiness.state).toBe("failed");

      manager.recover();
      await new Promise((resolve) => originalTimeout(resolve, 0));
      await settle();
      expect(attempts).toBe(3);
    } finally {
      manager.dispose();
      timer.mockRestore();
      clock.mockRestore();
    }
  });
