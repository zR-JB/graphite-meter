import { afterAll } from "bun:test";
import { resolve, sep } from "node:path";
import { expect, test } from "../browser/webview";

const host = "127.0.0.1";
const ports = { h1: 7256, h1tls: 7257, h3: 7259 };
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
  h3: `https://${host}:${ports.h3}`,
};

const backend = Bun.spawn([binary], {
  env: {
    ...process.env,
    GM_H1_ADDR: `${host}:${ports.h1}`,
    GM_H1_TLS_ADDR: `${host}:${ports.h1tls}`,
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

const deadline = Date.now() + 30_000;
while (true) {
  if (backend.exitCode !== null)
    throw new Error(`E2E server exited with ${backend.exitCode}`);
  try {
    const response = await fetch(`${origins["h1-clear"]}/preflight`);
    if (response.ok) break;
  } catch {}
  if (Date.now() >= deadline) throw new Error("E2E server readiness timed out");
  await Bun.sleep(50);
}

export const harnessOrigin = `http://${host}:${harness.port}`;
afterAll(async () => {
  harness.stop(true);
  backend.kill();
  await backend.exited;
});

export { expect, test };
