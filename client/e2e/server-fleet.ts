import { afterAll } from "bun:test";

export interface FleetServer {
  id: string;
  name: string;
  url: string;
  http: string;
  h2: string;
  h3: string;
}
export const fixturePassword = "local-multi-server-fixture";

// Separate real processes own their upload stores, admission budgets and login sessions.
export async function createServerFleet({
  mutualCatalogue = false,
}: { mutualCatalogue?: boolean } = {}) {
  const base =
    Number(process.env.GM_E2E_PORT_BASE ?? 7256) + (mutualCatalogue ? 96 : 64);
  if (base > 65510)
    throw new Error("E2E port range has no room for the server catalogue");
  const host = "127.0.0.1";
  const fleet: FleetServer[] = [
    "Home",
    "Frankfurt",
    "Amsterdam",
    "Helsinki",
    ...(mutualCatalogue ? [] : ["Private"]),
  ].map((name, i) => ({
    id: i === 0 ? "self" : `server-${i}`,
    name,
    url: `https://${host}:${base + i * 4 + 1}`,
    http: `http://${host}:${base + i * 4}`,
    h2: `https://${host}:${base + i * 4 + 2}`,
    h3: `https://${host}:${base + i * 4 + 3}`,
  }));
  const binary = process.env.GM_E2E_SERVER_BIN!;
  const hash = Bun.spawnSync([binary, "hash-password"], {
    stdin: Buffer.from(`${fixturePassword}\n${fixturePassword}\n`),
    stdout: "pipe",
    stderr: "pipe",
  });
  if (hash.exitCode !== 0)
    throw new Error("could not generate fixture password hash");
  const passwordHash = hash.stdout.toString().trim();
  process.env.BUN_CHROME_ARGS = [
    process.env.BUN_CHROME_ARGS,
    // Self-signed QUIC fixtures require forcing; cold Alt-Svc startup needs separate coverage.
    // Bun shares Chrome across files, so include the standalone transport fixture.
    `--origin-to-force-quic-on=${[`${host}:${Number(process.env.GM_E2E_PORT_BASE ?? 7256) + 3}`, ...fleet.map((server) => new URL(server.h3).host)].join(",")}`,
    `--ignore-certificate-errors-spki-list=${process.env.GM_E2E_SPKI!}`,
    "--test-third-party-cookie-phaseout",
  ]
    .filter(Boolean)
    .join(" ");

  const environment = Object.fromEntries(
    Object.entries(process.env).filter(
      ([key]) =>
        !key.startsWith("GM_AUTH_") && !key.startsWith("GM_SERVER_CATALOG"),
    ),
  );
  const processes = fleet.map((server, i) =>
    Bun.spawn([binary], {
      env: {
        ...environment,
        GM_SERVER_NAME: server.name,
        GM_SERVER_LOCATION: "Loopback fixture",
        GM_H1_ADDR: new URL(server.http).host,
        GM_H1_TLS_ADDR: new URL(server.url).host,
        GM_H2_ADDR: new URL(server.h2).host,
        GM_H3_ADDR: new URL(server.h3).host,
        GM_TLS_CERT: process.env.GM_E2E_TLS_CERT!,
        GM_TLS_KEY: process.env.GM_E2E_TLS_KEY!,
        ...(i === 1
          ? { GM_ADVERTISED_NATIVE_ENDPOINTS: "http1-tls,http2,http3" }
          : {}),
        ...(mutualCatalogue
          ? {
              GM_SERVER_CATALOG: JSON.stringify({
                servers: fleet
                  .filter((peer) => peer.id !== server.id)
                  .map(({ id, name, url }) => ({
                    id: id === "self" ? "home-peer" : id,
                    name,
                    url,
                  })),
              }),
            }
          : i === 0
            ? {
                GM_SERVER_CATALOG: JSON.stringify({
                  servers: fleet
                    .slice(1)
                    .map(({ id, name, url }) => ({ id, name, url })),
                }),
              }
            : i === 3
              ? { GM_SERVER_CATALOG: JSON.stringify([fleet[1].url]) }
              : {}),
        ...(i === 4
          ? {
              GM_SERVER_CATALOG: JSON.stringify([fleet[1].url]),
              GM_AUTH_MODE: "password",
              GM_ADVERTISED_NATIVE_ENDPOINTS: "http1-tls,http2,http3",
              GM_AUTH_PUBLIC_URL: server.url,
              GM_AUTH_PASSWORD_HASH: passwordHash,
            }
          : { GM_AUTH_MODE: "off" }),
      },
      stdout: "ignore",
      stderr: Bun.file(`/tmp/graphite-meter-e2e-${base}-${i}.log`),
    }),
  );
  let closed = false;
  async function stopFleetServer(id: string) {
    const index = fleet.findIndex((server) => server.id === id);
    if (index < 1) throw new Error("Only a fixture peer can be stopped");
    // Abrupt failure must interrupt live streams; graceful shutdown deliberately drains them.
    processes[index].kill("SIGKILL");
    await processes[index].exited;
  }
  async function close() {
    if (closed) return;
    closed = true;
    processes.forEach((process) => process.kill());
    await Promise.all(processes.map((process) => process.exited));
  }
  afterAll(close);
  try {
    await Promise.all(
      fleet.map(async (server, i) => {
        const deadline = Date.now() + 15_000;
        while (Date.now() < deadline) {
          if (processes[i].exitCode !== null)
            throw new Error(`${server.name} exited during setup`);
          try {
            const response = await fetch(`${server.http}/preflight`, {
              signal: AbortSignal.timeout(500),
              redirect: "manual",
            });
            if (
              i === 4
                ? response.status === 401 ||
                  response.status === 403 ||
                  response.status === 307 ||
                  response.status === 308
                : response.ok
            )
              return;
          } catch {}
          await Bun.sleep(50);
        }
        throw new Error(`${server.name} did not become ready`);
      }),
    );
  } catch (error) {
    await close();
    throw error;
  }
  return { fleet, stopFleetServer };
}
