import { afterAll } from "bun:test";
import { resolve, sep } from "node:path";
import { expect, test } from "../browser/webview";

const host = process.env.GM_BENCH_HOST ?? "127.0.0.1";
const ports = { h1: 7246, h1tls: 7247, h2: 7248, h3: 7249 };
const spki = process.env.GM_BENCH_SPKI;
if (!spki) throw new Error("GM_BENCH_SPKI is required for Chromium QUIC");
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

const command = ["go", "run", "./cmd/graphite-meter"];
if (process.env.GM_BENCH_NETNS)
  command.unshift("ip", "netns", "exec", process.env.GM_BENCH_NETNS);
const backend = Bun.spawn(command, {
  cwd: "../go",
  env: {
    ...process.env,
    GM_H1_ADDR: `${host}:${ports.h1}`,
    GM_H1_TLS_ADDR: `${host}:${ports.h1tls}`,
    GM_H2_ADDR: `${host}:${ports.h2}`,
    GM_H3_ADDR: `${host}:${ports.h3}`,
    GM_TLS_CERT: process.env.GM_BENCH_TLS_CERT ?? "../.dev-certs/localhost.pem",
    GM_TLS_KEY:
      process.env.GM_BENCH_TLS_KEY ?? "../.dev-certs/localhost-key.pem",
    GM_VERBOSE: "1",
    GM_MAX_ACTIVE_MEASUREMENTS: "4096",
    GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT: "4096",
    GM_MAX_ACTIVE_SESSIONS: "4096",
    GM_MAX_SESSIONS_PER_CLIENT: "4096",
    GM_MAX_CONNECTIONS: "8192",
    GM_MAX_CONNECTIONS_PER_CLIENT: "8192",
  },
  stdout: "inherit",
  stderr: "inherit",
});

const root = resolve(".e2e-dist");
const harness = Bun.serve({
  hostname: "127.0.0.1",
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

const deadline = Date.now() + 180_000;
while (true) {
  if (backend.exitCode !== null)
    throw new Error(`benchmark server exited with ${backend.exitCode}`);
  try {
    const response = await fetch(`${origins["h1-clear"]}/preflight`);
    if (response.ok) break;
  } catch {}
  if (Date.now() >= deadline)
    throw new Error("benchmark server readiness timed out");
  await Bun.sleep(100);
}

export const harnessOrigin = `http://127.0.0.1:${harness.port}`;
afterAll(async () => {
  harness.stop(true);
  backend.kill();
  await backend.exited;
});
export { expect, test };
