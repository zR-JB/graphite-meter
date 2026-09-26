import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";

export const host = "127.0.0.1";
export const logs = resolve(import.meta.dir, "../test-results/servers");

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

// Below the Linux ephemeral range, so outgoing connections cannot take a port.
const taken = new Set<number>();
function freePort(): number {
  const port = 20_000 + Math.floor(Math.random() * 12_000);
  try {
    if (taken.has(port)) throw new Error("port reused");
    Bun.listen({ hostname: host, port, socket: { data() {} } }).stop(true);
    taken.add(port);
    return port;
  } catch {
    return freePort();
  }
}

export function describe(id: string, name: string): Server {
  const [h1, tls, h2, h3] = [0, 1, 2, 3].map(freePort);
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
      ([key]) =>
        !key.startsWith("GM_AUTH_") && !key.startsWith("GM_SERVER_CATALOG"),
    ),
  );

export async function launch(
  { bin, cert, key }: Launch,
  server: Server,
  env: Record<string, string> = {},
) {
  await mkdir(logs, { recursive: true });
  const child = Bun.spawn([bin], {
    env: {
      ...inherited(),
      GM_SERVER_NAME: server.name,
      GM_SERVER_LOCATION: "Loopback fixture",
      GM_H1_ADDR: new URL(server.http).host,
      GM_H1_TLS_ADDR: new URL(server.url).host,
      GM_H2_ADDR: new URL(server.h2).host,
      GM_H3_ADDR: new URL(server.h3).host,
      GM_TLS_CERT: cert,
      GM_TLS_KEY: key,
      GM_AUTH_MODE: "off",
      GM_MAX_OPERATION_DURATION: "20s",
      GM_MAX_SESSION_DURATION: "1m",
      GM_MAX_ACTIVE_MEASUREMENTS: "32",
      GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT: "32",
      GM_MAX_ACTIVE_SESSIONS: "12",
      GM_MAX_SESSIONS_PER_CLIENT: "12",
      ...env,
    },
    stdout: "ignore",
    stderr: Bun.file(resolve(logs, `${server.name}-${Date.now()}.log`)),
  });
  const deadline = Date.now() + 15_000;
  while (true) {
    if (child.exitCode !== null)
      throw new Error(`${server.name} exited with ${child.exitCode}`);
    const status = await fetch(`${server.http}/preflight`, {
      redirect: "manual",
      signal: AbortSignal.timeout(500),
    }).then(
      (response) => response.status,
      () => 0,
    );
    if (status > 0 && status < 500) return child;
    if (Date.now() > deadline) {
      child.kill();
      throw new Error(`${server.name} did not become ready`);
    }
    await Bun.sleep(50);
  }
}
