import { beforeEach, afterEach, expect, spyOn, test } from "bun:test";
import { DEFAULT_CONFIG } from "../state/defaults";
import { testPreparedPaths } from "../runner/test-helpers.test";
import type { ConnectionPreparation } from "../runner/real/prepare";
import { ServerConnections, type ServerConnectionView } from "./connections";

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
      delay === 8000 ? 0 : delay,
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
