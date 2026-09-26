import { afterAll, afterEach, beforeAll, expect, spyOn, test } from "bun:test";
import type { RunnerConfig } from "./contract";
import type { ConnectionHost, ServerConnection } from "./connection";
import { CONNECTION_FRESH_MS } from "./paths";
import type { ConnectionPreparation } from "./real/prepare";
import type { ServerEntry } from "../servers/catalog";
import {
  ServerAuthenticationRequired,
  type ServerCredentials,
} from "../servers/credentials";
import { originLimiter } from "../servers/originLimiter";
import { DEFAULT_CONFIG } from "../state/defaults";
import { stubGlobals } from "../test-helpers.testutil";
import { TEST_BUILD_TOKENS, testPreparedPaths } from "./test-helpers.testutil";

let restoreBuild: () => void;
let Connection: typeof ServerConnection;
let PreflightUnavailable: typeof import("./real/prepare").PreflightUnavailableError;
beforeAll(async () => {
  restoreBuild = stubGlobals(TEST_BUILD_TOKENS);
  ({ ServerConnection: Connection } = await import("./connection"));
  ({ PreflightUnavailableError: PreflightUnavailable } =
    await import("./real/prepare"));
});
afterAll(() => restoreBuild());
const open: ServerConnection[] = [];
afterEach(() => {
  for (const connection of open.splice(0)) connection.close();
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => (resolve = done));
  return { promise, resolve };
}
async function until(done: () => boolean, turns = 100): Promise<void> {
  for (let turn = 0; turn < turns && !done(); turn++)
    await new Promise((resolve) => setTimeout(resolve, 0));
}
const settle = () => until(() => false, 10);
function evidence(generation = "gen-a") {
  const paths = testPreparedPaths();
  paths.discovery.generation = generation;
  paths.throughput.generation = generation;
  paths.latency!.generation = generation;
  return paths;
}
function preparation(
  config: RunnerConfig = DEFAULT_CONFIG,
  paths = evidence(),
): ConnectionPreparation {
  const role = <P>(selection: string, path: P) => ({
    selection,
    state: "verified" as const,
    path,
  });
  return {
    discovery: paths.discovery,
    validation: {
      throughput: role(config.transports.throughputTarget, paths.throughput),
      latency: role(config.transports.latencyTarget, paths.latency),
    },
  };
}
const monitor = (stopped = () => {}) => ({
  start() {},
  stop: stopped,
  onEvent() {},
});
function connect(
  options: Partial<ConnectionHost> & {
    id?: string;
    credentials?: Partial<ServerCredentials>;
    config?: RunnerConfig | null;
  } = {},
): ServerConnection {
  const id = options.id ?? "self";
  const server: ServerEntry = {
    id,
    name: id,
    url: "http://meter.test",
  };
  const connection = new Connection(
    server,
    { kind: "public", ...options.credentials, server },
    {
      discover: async () => evidence().discovery,
      prepare: async (config) => preparation(config),
      limiter: originLimiter(),
      active: () => false,
      metadata: () => false,
      idle: () => false,
      publish() {},
      idleEvent() {},
      ...options,
    },
  );
  open.push(connection);
  if (options.config !== null)
    connection.select(options.config ?? structuredClone(DEFAULT_CONFIG));
  return connection;
}

test("a failed role keeps the other role's verified path", async () => {
  const connection = connect({
    prepare: async (config, _previous, roles) => {
      if (roles.includes("latency")) throw new Error("offline");
      return preparation(config);
    },
  });
  await connection.check();
  expect(connection.view.validation.throughput.state).toBe("verified");
  expect(connection.view.validation.latency).toMatchObject({
    state: "failed",
    message: "Connection check failed",
  });
  expect(connection.view.readiness).toBe("failed");
  expect(connection.paths()).toBeNull();
});

test("the view fails throughput without upload checkpoints but keeps its probe evidence", async () => {
  const paths = evidence();
  paths.discovery.uploadCheckpoint = false;
  const connection = connect({
    discover: async () => paths.discovery,
    prepare: async (config) => preparation(config, paths),
  });
  await connection.check();
  expect(connection.view).toMatchObject({
    readiness: "failed",
    message: expect.stringContaining("checkpoint"),
  });
  const { throughput, latency } = connection.view.validation;
  expect(throughput).toMatchObject({ state: "failed", path: paths.throughput });
  expect(latency.state).toBe("verified");
});

test("an intent change cancels only its role and discards the late result", async () => {
  const held = { throughput: deferred<void>(), latency: deferred<void>() };
  const signals: Partial<Record<string, AbortSignal>> = {};
  let stopped = 0;
  const connection = connect({
    prepare: async (config, _previous, [role], signal) => {
      const first = !signals[role];
      signals[role] ??= signal;
      if (first) await held[role].promise;
      const late = first && role === "latency";
      return {
        ...preparation(config),
        idle: late ? monitor(() => void stopped++) : undefined,
      };
    },
  });
  void connection.check();
  await settle();
  const config = structuredClone(DEFAULT_CONFIG);
  config.transports.latencyTarget = "transport:websocket";
  connection.select(config);
  expect(signals.throughput!.aborted).toBe(false);
  expect(signals.latency!.aborted).toBe(true);
  expect(connection.view.validation.latency.state).toBe("stale");
  held.throughput.resolve();
  await connection.check();
  const current = connection.view.validation.latency.path;
  held.latency.resolve();
  await settle();
  expect(stopped).toBe(1);
  expect(connection.view.validation.latency.path).toBe(current);
  expect(connection.view.validation.latency.selection).toBe(
    "transport:websocket",
  );
  expect(connection.view.readiness).toBe("ready");
});

test("a new server generation cancels an in-flight role before accepting replacement evidence", async () => {
  const held = deferred<ConnectionPreparation>();
  let generation = "gen-a";
  let block = false;
  let heldSignal: AbortSignal | undefined;
  const connection = connect({
    discover: async () => evidence(generation).discovery,
    prepare: async (config, _previous, [role], signal) => {
      if (!block || role !== "latency")
        return preparation(config, evidence(generation));
      heldSignal = signal;
      return held.promise;
    },
  });
  await connection.check();
  block = true;
  const old = connection.check({ force: true, role: "latency" });
  await settle();
  expect(heldSignal?.aborted).toBe(false);
  [generation, block] = ["gen-b", false];
  await connection.check({ force: true, role: "throughput" });
  await old;
  expect(heldSignal?.aborted).toBe(true);
  held.resolve(preparation());
  await settle();
  expect(connection.view.readiness).toBe("ready");
  expect(connection.view.validation.latency.path!.generation).toBe("gen-b");
});

test("sign-in is required by a wrapped refusal or an expired grant and never retried on its own", async () => {
  const refused = connect({
    id: "peer",
    discover: async (_signal, credentials) => {
      throw new PreflightUnavailable("preflight unavailable", {
        cause: new ServerAuthenticationRequired(credentials!.server),
      });
    },
  });
  await refused.check();
  expect(refused.view).toMatchObject({
    readiness: "sign-in",
    message: "Sign in to peer",
  });
  refused.resume();
  expect(refused.dueAt()).toBe(Infinity);
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  try {
    let active = false;
    const granted = connect({
      id: "peer",
      credentials: { kind: "grant", token: "t", expiresAt: 2000 },
      active: () => active,
    });
    await granted.check();
    expect(granted.view.readiness).toBe("ready");
    expect(granted.dueAt()).toBe(2000);
    clock.mockReturnValue(2001);
    active = true;
    granted.wake();
    await until(() => granted.dueAt() === Infinity);
    expect(granted.dueAt()).toBe(Infinity);
    expect(granted.view).toMatchObject({
      readiness: "sign-in",
      message: "Sign in to node-a",
    });
    expect(granted.paths()).toBeNull();
  } finally {
    clock.mockRestore();
  }
});

test("a required sign-in yields to the next check; failures back off 30 s then 60 s", async () => {
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  let offline = false;
  try {
    const connection = connect({
      discover: async () => {
        if (offline) throw new Error("offline");
        return evidence().discovery;
      },
    });
    await connection.check();
    connection.requireSignIn("Sign in again");
    expect(connection.view).toMatchObject({
      readiness: "sign-in",
      message: "Sign in again",
    });
    expect(connection.dueAt()).toBe(Infinity);
    offline = true;
    await connection.check();
    expect(connection.view).toMatchObject({
      readiness: "failed",
      message: "Connection check failed",
    });
    expect(connection.dueAt()).toBe(31_000);
    await connection.check();
    expect(connection.dueAt()).toBe(61_000);
    connection.resume();
    expect(connection.dueAt()).toBe(0);
  } finally {
    clock.mockRestore();
  }
});

test("equivalent intent reuses fresh paths; an expired reselection refreshes discovery and both roles", async () => {
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  let discoveries = 0;
  let probes = 0;
  try {
    const connection = connect({
      discover: async () => (discoveries++, evidence().discovery),
      prepare: async (config) => (probes++, preparation(config)),
    });
    await connection.check();
    const ready = connection.view;
    const { throughput, latency } = evidence();
    const config = structuredClone(DEFAULT_CONFIG);
    config.transports.throughputTarget = throughput.target.origin;
    config.transports.latencyTarget = latency!.target.origin;
    connection.select(config);
    await connection.check();
    expect(connection.view.readiness).toBe("ready");
    expect(connection.view.validation.throughput.path).toBe(
      ready.validation.throughput.path,
    );
    const equivalent = connection.view;
    connection.select(structuredClone(config));
    expect(connection.view).toBe(equivalent);
    connection.select(null);
    clock.mockReturnValue(1000 + CONNECTION_FRESH_MS + 1);
    connection.select(config);
    expect(connection.view.readiness).toBe("unchecked");
    await connection.check();
    expect(connection.view.readiness).toBe("ready");
    expect([discoveries, probes]).toEqual([2, 4]);
  } finally {
    clock.mockRestore();
  }
});

test("closing discards a pending check and stops its late monitor", async () => {
  const held = deferred<ConnectionPreparation>();
  let published = 0;
  let stopped = 0;
  const connection = connect({
    prepare: async (config, _previous, [role]) =>
      role === "latency" ? held.promise : preparation(config),
    publish: () => void published++,
  });
  const pending = connection.check();
  await settle();
  const before = published;
  connection.close();
  held.resolve({ ...preparation(), idle: monitor(() => void stopped++) });
  await pending;
  await settle();
  expect(published).toBe(before);
  expect(stopped).toBe(1);
});

test("background discovery leaves capacity for a selected server", async () => {
  const limiter = originLimiter();
  const background = deferred<void>();
  const started: string[] = [];
  const discover: ConnectionHost["discover"] = async (_signal, credentials) => {
    started.push(credentials!.server.id);
    if (credentials!.server.id !== "self") await background.promise;
    return evidence().discovery;
  };
  const shared = { limiter, discover, config: null, metadata: () => true };
  const slow = [
    connect({ ...shared, id: "a" }),
    connect({ ...shared, id: "b" }),
  ];
  for (const connection of slow) void connection.check();
  const selected = connect({ limiter, discover });
  await selected.check();
  expect(started).toEqual(["a", "self"]);
  expect(selected.view.readiness).toBe("ready");
  expect(slow[0].view.metadataChecking).toBe(true);
  background.resolve();
});
