import { closeSync, openSync } from "node:fs";
import { createConnection, createServer, type Socket } from "node:net";
import { mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

export const host = "127.0.0.1";
export const logs = resolve("test-results/servers");

export interface Server {
  id: string;
  name: string;
  http: string;
  url: string;
  h2: string;
  h3: string;
}
export interface Launch {
  bin: string;
  cert: string;
  key: string;
}

/**
 * The network group's servers: an address each, so loopback's queues tell them apart, and fixed ports for Chrome's
 * QUIC flag. Their clear HTTP listens on ::1, which reaches them from an address other than the browser's.
 */
export const NETWORK_FLEET: Server[] = [11, 12, 13, 14].map((n, i) => {
  const at = (port: number) => `https://127.0.0.${n}:${port}`;
  return {
    id: i ? `s${i + 1}` : "self",
    name: `S${i + 1}`,
    http: `http://[::1]:${7246 + 10 * i}`,
    url: at(7247),
    h2: at(7248),
    h3: at(7249),
  };
});

// Below the Linux ephemeral range, so no outgoing connection takes one first; reservation files split the ports.
const FIRST_PORT = 20_000;
const PORTS = 12_000;
const PORT_TRIES = 100;
export async function freePort(): Promise<number> {
  for (let tries = 0; tries < PORT_TRIES; tries++) {
    const port = FIRST_PORT + Math.floor(Math.random() * PORTS);
    try {
      closeSync(openSync(join(tmpdir(), `gm-port-${port}`), "wx"));
      Bun.listen({ hostname: host, port, socket: { data() {} } }).stop(true);
      (await Bun.udpSocket({ hostname: host, port })).close();
      return port;
    } catch (error) {
      const { code } = error as NodeJS.ErrnoException;
      if (code !== "EEXIST" && code !== "EADDRINUSE") throw error;
    }
  }
  throw new Error(
    `No free TCP and UDP port on ${host} in ${PORT_TRIES} tries of ${FIRST_PORT}-${FIRST_PORT + PORTS - 1}`,
  );
}

export async function describe(id: string, name: string): Promise<Server> {
  const [h1, tls, h2, h3] = [
    await freePort(),
    await freePort(),
    await freePort(),
    await freePort(),
  ];
  return {
    id,
    name,
    http: `http://${host}:${h1}`,
    url: `https://${host}:${tls}`,
    h2: `https://${host}:${h2}`,
    h3: `https://${host}:${h3}`,
  };
}

const inherited = () =>
  Object.fromEntries(
    Object.entries(process.env).filter(
      ([key]) => !key.startsWith("GM_AUTH_") && !key.startsWith("GM_SERVER_CATALOG"),
    ),
  );

const LINK_BYTES_PER_SEC = 25e6;

/**
 * Starts `server`, closed when the server exits. On the `shared` address its HTTPS origin is behind a paced link and
 * its quotas fit every worker's browser; otherwise the network namespace shapes its link and its quotas are the defaults.
 */
export async function launch(
  { bin, cert, key }: Launch,
  server: Server,
  env: Record<string, string> = {},
  shared = true,
) {
  await mkdir(logs, { recursive: true });
  const tls = new URL(server.url);
  const listener = shared ? await freePort() : +tls.port;
  const unlink = shared ? await pace(+tls.port, listener, LINK_BYTES_PER_SEC) : () => {};
  const log = resolve(logs, `${server.name}-${Date.now()}.log`);
  const child = Bun.spawn([bin], {
    env: {
      ...inherited(),
      GM_SERVER_NAME: server.name,
      GM_SERVER_LOCATION: "Loopback fixture",
      GM_H1_ADDR: new URL(server.http).host,
      GM_H1_TLS_ADDR: `${tls.hostname}:${listener}`,
      GM_H1_TLS_PUBLIC_ORIGIN: server.url,
      GM_H2_ADDR: new URL(server.h2).host,
      GM_H3_ADDR: new URL(server.h3).host,
      GM_TLS_CERT: cert,
      GM_TLS_KEY: key,
      GM_AUTH_MODE: "off",
      GM_MAX_OPERATION_DURATION: "20s",
      GM_MAX_SESSION_DURATION: "1m",
      ...(shared && {
        // Each page's leases hold its runs' measurements from their start.
        GM_MAX_ACTIVE_MEASUREMENTS: "128",
        GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT: "128",
        GM_MAX_ACTIVE_SESSIONS: "32",
        GM_MAX_SESSIONS_PER_CLIENT: "32",
        // All workers' browsers share one address; Firefox neared the default 64 and hit NS_ERROR_NET_RESET.
        GM_MAX_CONNECTIONS_PER_CLIENT: "192",
      }),
      ...env,
    },
    stdout: "ignore",
    stderr: Bun.file(log),
  });
  void child.exited.then(unlink);
  const deadline = Date.now() + 15_000;
  while (true) {
    if (child.exitCode !== null) throw new Error(`${server.name} exited with ${child.exitCode}`);
    const status = await fetch(`${server.http}/preflight`, {
      redirect: "manual",
      signal: AbortSignal.timeout(500),
    }).then(
      (response) => response.status,
      () => 0,
    );
    if (status > 0 && status < 500) return Object.assign(child, { log });
    if (Date.now() > deadline) {
      child.kill();
      await child.exited;
      throw new Error(`${server.name} did not become ready`);
    }
    await Bun.sleep(50);
  }
}

/** A TCP proxy from `listen` to `target` holding each direction to `bytesPerSec`, a link so no browser's network thread saturates; resolves with its close. */
export async function pace(
  listen: number,
  target: number,
  bytesPerSec: number,
): Promise<() => void> {
  const link = () => {
    let free = 0;
    return (from: Socket, to: Socket) =>
      from.on("data", (chunk: Buffer) => {
        const now = performance.now();
        free = Math.max(free, now) + (chunk.length * 1000) / bytesPerSec;
        const written = to.write(chunk);
        if (written && free - now < 1) return;
        from.pause();
        const drained = written ? null : new Promise((resolve) => to.once("drain", resolve));
        void Promise.all([Bun.sleep(free - now), drained]).then(() => from.resume());
      });
  };
  const [down, up] = [link(), link()];
  const open = new Set<Socket>();
  const proxy = createServer((client) => {
    const server = createConnection({ host, port: target });
    open.add(client).add(server);
    for (const [from, to] of [
      [client, server],
      [server, client],
    ] as const) {
      from.on("error", () => to.destroy());
      from.on("close", () => {
        open.delete(from);
        to.destroy();
      });
    }
    up(client, server);
    down(server, client);
  });
  await new Promise<void>((resolve, reject) =>
    proxy.once("error", reject).listen(listen, host, resolve),
  );
  return () => {
    proxy.close();
    for (const socket of open) socket.destroy();
  };
}
