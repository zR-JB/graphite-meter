import { afterAll } from "bun:test";
import { resolve, sep } from "node:path";
import { expect, test } from "../browser/webview";

const host = "127.0.0.1";
const portBase = Number(process.env.GM_E2E_PORT_BASE ?? 7256);
if (!Number.isInteger(portBase) || portBase < 1024 || portBase > 65532)
  throw new Error("GM_E2E_PORT_BASE must be an integer from 1024 to 65532");
const ports = {
  h1: portBase,
  h1tls: portBase + 1,
  h2: portBase + 2,
  h3: portBase + 3,
};
const serverName = `e2e-${crypto.randomUUID()}`;
const binary = process.env.GM_E2E_SERVER_BIN;
const spki = process.env.GM_E2E_SPKI;
if (!binary || !spki)
  throw new Error("run transport tests with `just client-e2e`");

process.env.BUN_CHROME_ARGS = [
  process.env.BUN_CHROME_ARGS,
  `--ignore-certificate-errors-spki-list=${spki}`,
  `--origin-to-force-quic-on=${host}:${ports.h3}`,
]
  .filter(Boolean)
  .join(" ");

export const origins = {
  "h1-clear": `http://${host}:${ports.h1}`,
  "h1-tls": `https://${host}:${ports.h1tls}`,
  h2: `https://${host}:${ports.h2}`,
  h3: `https://${host}:${ports.h3}`,
};

const backend = Bun.spawn([binary], {
  env: {
    ...process.env,
    GM_SERVER_NAME: serverName,
    GM_H1_ADDR: `${host}:${ports.h1}`,
    GM_H1_TLS_ADDR: `${host}:${ports.h1tls}`,
    GM_H2_ADDR: `${host}:${ports.h2}`,
    GM_H3_ADDR: `${host}:${ports.h3}`,
    GM_TLS_CERT: process.env.GM_E2E_TLS_CERT ?? "",
    GM_TLS_KEY: process.env.GM_E2E_TLS_KEY ?? "",
  },
  stdout: "inherit",
  stderr: "inherit",
});

const root = resolve(".e2e-dist");
const harness = Bun.serve({
  hostname: host,
  port: 0,
  async fetch(request) {
    const relative = decodeURIComponent(new URL(request.url).pathname).replace(
      /^\/+/,
      "",
    );
    const path = resolve(root, relative);
    if (path !== root && !path.startsWith(root + sep))
      return new Response("forbidden", { status: 403 });
    const file = Bun.file(path);
    if (!(await file.exists()))
      return new Response("not found", { status: 404 });
    return new Response(file, { headers: { "cache-control": "no-store" } });
  },
});

let closed = false;
async function closeFixtures() {
  if (closed) return;
  closed = true;
  harness.stop(true);
  backend.kill();
  await backend.exited;
}
afterAll(closeFixtures);

try {
  const deadline = Date.now() + 30_000;
  while (true) {
    if (backend.exitCode !== null)
      throw new Error(`E2E server exited with ${backend.exitCode}`);
    try {
      const response = await fetch(`${origins["h1-clear"]}/preflight`, {
        signal: AbortSignal.timeout(1000),
      });
      if (response.ok && (await response.json()).server?.name === serverName)
        break;
    } catch {}
    if (Date.now() >= deadline)
      throw new Error("E2E server readiness timed out");
    await Bun.sleep(50);
  }
} catch (error) {
  await closeFixtures();
  throw error;
}

export const harnessOrigin = `http://${host}:${harness.port}`;

export { expect, test };
