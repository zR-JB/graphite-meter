import { X509Certificate, createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";
import { describe, host, launch } from "../e2e/servers";

const bin =
  process.env.GM_E2E_SERVER_BIN ??
  resolve(import.meta.dir, "../test-results/graphite-meter");
if (!(await Bun.file(bin).exists()))
  throw new Error(`${bin} is missing; build it with mise run e2e`);
const chrome = process.env.BUN_CHROME_PATH;
const expected = process.env.GM_EXPECTED_CHROME_VERSION;
if (expected) {
  const version = Bun.spawnSync([chrome ?? "chrome", "--version"]);
  const actual = version.stdout.toString().trim();
  if (actual !== `Google Chrome for Testing ${expected}`)
    throw new Error(`Chrome is ${actual}; expected ${expected}`);
}

const dir = await mkdtemp(join(tmpdir(), "gm-e2e-"));
const cert = join(dir, "cert.pem");
const key = join(dir, "key.pem");
const openssl = Bun.spawnSync(
  ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1"]
    .concat(["-keyout", key, "-out", cert, "-subj", `/CN=${host}`])
    .concat(["-addext", `subjectAltName=IP:${host},DNS:localhost`]),
  { stderr: "ignore" },
);
if (openssl.exitCode !== 0) throw new Error("openssl could not create a cert");
const spki = createHash("sha256")
  .update(
    new X509Certificate(await Bun.file(cert).text()).publicKey.export({
      type: "spki",
      format: "der",
    }),
  )
  .digest("base64");
const password = "local-e2e-fixture";
const hash = Bun.spawnSync([bin, "hash-password"], {
  stdin: Buffer.from(`${password}\n${password}\n`),
});
if (hash.exitCode !== 0) throw new Error("could not hash the fixture password");

const fleet = [
  describe("self", "Home"),
  describe("server-1", "Frankfurt"),
  describe("server-2", "Amsterdam"),
  describe("server-3", "Helsinki"),
  describe("server-4", "Private"),
];
const [, frankfurt, , , locked] = fleet;
const peers = fleet.slice(1).map(({ id, name, url }) => ({ id, name, url }));
const native = { GM_ADVERTISED_NATIVE_ENDPOINTS: "http1-tls,http2,http3" };
const environments: Record<string, string>[] = [
  { GM_SERVER_CATALOG: JSON.stringify({ servers: peers }) },
  native,
  {},
  { GM_SERVER_CATALOG: JSON.stringify([frankfurt.url]) },
  {
    ...native,
    GM_SERVER_CATALOG: JSON.stringify([frankfurt.url]),
    GM_AUTH_MODE: "password",
    GM_AUTH_PUBLIC_URL: locked.url,
    GM_AUTH_PASSWORD_HASH: hash.stdout.toString().trim(),
  },
];
const launched = { bin, cert, key };
const children = await Promise.all(
  fleet.map((server, i) => launch(launched, server, environments[i])),
);

const root = resolve(import.meta.dir, "../.e2e-dist");
const harness = Bun.serve({
  hostname: host,
  port: 0,
  async fetch(request) {
    const path = resolve(root, `.${new URL(request.url).pathname}`);
    const file = Bun.file(path);
    if (!path.startsWith(root + sep) || !(await file.exists()))
      return new Response("not found", { status: 404 });
    return new Response(file);
  },
});

async function stop() {
  harness.stop(true);
  children.forEach((child) => child.kill());
  await Promise.all(children.map((child) => child.exited));
  await rm(dir, { recursive: true, force: true });
}
for (const signal of ["SIGINT", "SIGTERM"] as const)
  process.on(signal, () => stop().then(() => process.exit(130)));

const started = performance.now();
const command = process.argv.slice(2);
const suite = Bun.spawn(
  command.length ? command : [process.execPath, "run", "test:e2e"],
  {
    cwd: resolve(import.meta.dir, ".."),
    stdio: ["inherit", "inherit", "inherit"],
    env: {
      ...process.env,
      GM_E2E: JSON.stringify({ fleet, password, harness: harness.url.origin }),
      GM_E2E_LAUNCH: JSON.stringify(launched),
      BUN_CHROME_ARGS: [
        process.env.BUN_CHROME_ARGS ?? "",
        `--ignore-certificate-errors-spki-list=${spki}`,
        `--origin-to-force-quic-on=${fleet.map((s) => new URL(s.h3).host)}`,
        "--test-third-party-cookie-phaseout",
      ].join(" "),
    },
  },
);
const code = await suite.exited;
console.log(
  `e2e suite: ${((performance.now() - started) / 1000).toFixed(1)} s`,
);
await stop();
process.exit(code);
