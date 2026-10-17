import { X509Certificate, createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { mkdtemp, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { describe, host, launch, logs } from "../e2e/servers";

// Paths resolve from the client folder, which the suite runs in.
const bin = process.env.GM_E2E_SERVER_BIN ?? resolve("../rust/target/ci/graphite-meter-server");
if (!(await Bun.file(bin).exists())) throw new Error(`${bin} is missing; mise run e2e-server builds it`);
// CI runs the browsers its Plan job selects; a local run takes whichever is installed.
for (const [path, expected, name] of [
  [process.env.BUN_CHROME_PATH ?? "chrome", process.env.GM_EXPECTED_CHROME_VERSION, "Google Chrome for Testing"],
  [process.env.GM_FIREFOX_PATH ?? "firefox", process.env.GM_EXPECTED_FIREFOX_VERSION, "Mozilla Firefox"],
] as const) {
  if (!expected) continue;
  const actual = Bun.spawnSync([path, "--version"]).stdout.toString().trim();
  if (actual !== `${name} ${expected}`) throw new Error(`${actual} is not ${name} ${expected}`);
}

const dir = await mkdtemp(join(tmpdir(), "gm-e2e-"));
// Everything the suite starts inherits it: servers, workers, browsers and their profiles, and port reservations.
process.env.TMPDIR = dir;

// A worker can exit without its cleanup hooks, so whatever still runs with this directory is stopped (Linux /proc).
async function ours() {
  const pids = (await readdir("/proc").catch(() => [])).filter((pid) => /^\d+$/.test(pid) && +pid !== process.pid);
  return pids.map(Number).filter((pid) => {
    try {
      const environ = `\0${readFileSync(`/proc/${pid}/environ`, "latin1")}`;
      return environ.includes(`\0TMPDIR=${dir}\0`) || readFileSync(`/proc/${pid}/cmdline`, "latin1").includes(dir);
    } catch {
      return false;
    }
  });
}

let stopping: Promise<void> | undefined;
let signaled: number | undefined;
function stop() {
  return (stopping ??= (async () => {
    for (const signal of ["SIGTERM", "SIGKILL"] as const) {
      for (const pid of await ours())
        try {
          process.kill(pid, signal);
        } catch {}
      for (let wait = 0; wait < 50 && (await ours()).length; wait++) await Bun.sleep(100);
    }
    await rm(dir, { recursive: true, force: true });
  })());
}
for (const [signal, code] of [
  ["SIGINT", 130],
  ["SIGTERM", 143],
] as const)
  process.on(signal, () => {
    signaled ??= code;
    void stop().then(() => process.exit(signaled));
  });

let status = 1;
try {
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
    .update(new X509Certificate(await Bun.file(cert).text()).publicKey.export({ type: "spki", format: "der" }))
    .digest("base64");
  const password = "local-e2e-fixture";
  const hash = Bun.spawnSync([bin, "hash-password"], { stdin: Buffer.from(`${password}\n${password}\n`) });
  if (hash.exitCode !== 0) throw new Error("could not hash the fixture password");
  const passwordHash = hash.stdout.toString().trim();

  const fleet = [
    await describe("self", "Home"),
    await describe("server-1", "Frankfurt"),
    await describe("server-2", "Amsterdam"),
    await describe("server-3", "Helsinki"),
    await describe("server-4", "Private"),
  ] as const;
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
      GM_AUTH_PASSWORD_HASH: passwordHash,
    },
  ];
  const launched = { bin, cert, key };
  await rm(logs, { recursive: true, force: true });
  await Promise.all(fleet.map((server, i) => launch(launched, server, environments[i])));

  const started = performance.now();
  const command = process.argv.slice(2);
  // A Firefox worker weighs more than Chrome's; three of them starve a CI runner's measurements.
  const workers = process.env.GM_E2E_BROWSER === "firefox" ? 2 : 3;
  const suite = Bun.spawn(command.length ? command : [process.execPath, "run", "test:e2e", `--parallel=${workers}`], {
    stdio: ["inherit", "inherit", "inherit"],
    env: {
      ...process.env,
      GM_E2E: JSON.stringify({ fleet, password, passwordHash }),
      GM_E2E_LAUNCH: JSON.stringify(launched),
      BUN_CHROME_ARGS: [
        process.env.BUN_CHROME_ARGS ?? "",
        `--ignore-certificate-errors-spki-list=${spki}`,
        `--origin-to-force-quic-on=${fleet.map((s) => new URL(s.h3).host)}`,
        "--test-third-party-cookie-phaseout",
      ].join(" "),
    },
  });
  status = await suite.exited;
  console.log(`e2e suite: ${((performance.now() - started) / 1000).toFixed(1)} s`);
} finally {
  await stop();
}
process.exit(signaled ?? status);
