// Throughput benchmark, run by `just bench-throughput`. Never part of ci: it
// takes hours and measures this machine rather than the code's correctness.
import { defineConfig } from "@playwright/test";

/** Where the server binds. Rig B overrides it with the namespace address. */
const HOST = process.env.GM_BENCH_HOST ?? "127.0.0.1";
/** Prefix that puts the server in a network namespace under the shaped rig. */
const NETNS = process.env.GM_BENCH_NETNS
  ? `ip netns exec ${process.env.GM_BENCH_NETNS} `
  : "";

const ports = { h1: 7246, h1tls: 7247, h2: 7248, h3: 7249 };

/** Base64 SHA-256 of the dev leaf's SubjectPublicKeyInfo. No default: a stale
 *  pin fails as a QUIC connection error rather than as a configuration error.
 *  Derive it from the cert this run serves:
 *
 *    openssl x509 -in ../.dev-certs/localhost.pem -pubkey -noout \
 *      | openssl pkey -pubin -outform der \
 *      | openssl dgst -sha256 -binary | openssl enc -base64
 */
const SPKI = process.env.GM_BENCH_SPKI;
/** Mozilla's own binary, not a distribution package: a distribution Firefox
 *  build may ship prefs that change socket buffers and cache ceilings, which
 *  measures the distribution rather than the browser. Env-only, and the project
 *  is skipped without it. */
const MOZ_FIREFOX = process.env.GM_BENCH_FIREFOX;

export const origins = {
  "h1-clear": `http://${HOST}:${ports.h1}`,
  "h1-tls": `https://${HOST}:${ports.h1tls}`,
  h2: `https://${HOST}:${ports.h2}`,
  h3: `https://${HOST}:${ports.h3}`,
};

// A benchmark studies throughput, not admission refusal, and every lane here
// shares one address. Mirrors what the saturation harness lifts for the same reason.
const serverEnv = {
  GM_H1_ADDR: `${HOST}:${ports.h1}`,
  GM_H1_TLS_ADDR: `${HOST}:${ports.h1tls}`,
  GM_H2_ADDR: `${HOST}:${ports.h2}`,
  GM_H3_ADDR: `${HOST}:${ports.h3}`,
  GM_TLS_CERT: process.env.GM_BENCH_TLS_CERT ?? "../.dev-certs/localhost.pem",
  GM_TLS_KEY: process.env.GM_BENCH_TLS_KEY ?? "../.dev-certs/localhost-key.pem",
  GM_VERBOSE: "1",
  GM_MAX_ACTIVE_MEASUREMENTS: "4096",
  GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT: "4096",
  GM_MAX_SESSIONS_PER_CLIENT: "4096",
  GM_MAX_CONNECTIONS: "8192",
  GM_MAX_CONNECTIONS_PER_CLIENT: "8192",
};

/** A project whose prerequisite env is missing is dropped rather than run with a
 *  guessed value, and says so: a wrong pin or a wrong binary reads as a slow or
 *  zero-byte cell, which is worse than an absent one. Playwright then rejects
 *  `--project=<name>` by name, which is the clear error. */
function requireEnv<T>(
  name: string,
  value: string | undefined,
  project: T,
): T[] {
  if (value) return [project];
  console.warn(`bench: ${name} is unset, skipping that project`);
  return [];
}

export default defineConfig({
  testDir: "./bench",
  testMatch: "**/*.bench.pw.ts",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: "list",
  // A cell is warmup plus measurement plus teardown, repeated within one test.
  timeout: 30 * 60_000,
  // Covers h1-tls and h2 for both engines; QUIC needs the SPKI pin above.
  use: {
    baseURL: "http://127.0.0.1:5173",
    trace: "off",
    ignoreHTTPSErrors: true,
  },
  projects: [
    ...requireEnv("GM_BENCH_SPKI", SPKI, {
      name: "chromium",
      use: {
        browserName: "chromium" as const,
        // ignoreHTTPSErrors does not cover QUIC, so the certificate is trusted
        // by hash instead, and forcing QUIC skips the Alt-Svc bootstrap race.
        launchOptions: {
          args: [
            `--ignore-certificate-errors-spki-list=${SPKI}`,
            `--origin-to-force-quic-on=${HOST}:${ports.h3}`,
          ],
        },
      },
    }),
    // Playwright's own firefox is a patched build, so it is not evidence about
    // what a user's firefox does. This drives a real one over BiDi.
    ...requireEnv("GM_BENCH_FIREFOX", MOZ_FIREFOX, {
      name: "firefox-stock",
      use: {
        browserName: "firefox" as const,
        channel: "moz-firefox",
        launchOptions: {
          executablePath: MOZ_FIREFOX,
          // QUIC ignores ignoreHTTPSErrors, so h3 and WebTransport need the
          // mkcert root actually trusted, as a system anchor.
          firefoxUserPrefs: {
            "security.enterprise_roots.enabled": true,
            "network.http.http3.enable": true,
            "network.http.http3.disable_when_third_party_roots_found": false,
          },
        },
      },
    }),
    {
      name: "firefox",
      use: {
        browserName: "firefox",
        // Firefox silently disables h3 when it finds a third-party root, which
        // a locally trusted certificate is.
        launchOptions: {
          firefoxUserPrefs: {
            // Same trust path as firefox-stock, so the two builds differ only
            // in the build.
            "security.enterprise_roots.enabled": true,
            "network.http.http3.enable": true,
            "network.http.http3.disable_when_third_party_roots_found": false,
            // Firefox defaults to a 32 KiB socket buffer, worth ~31% on
            // loopback. Off unless asked for, so matrix rows stay comparable.
            ...(process.env.GM_BENCH_FF_NETBUF
              ? {
                  "network.buffer.cache.size": Number(
                    process.env.GM_BENCH_FF_NETBUF,
                  ),
                }
              : {}),
          },
        },
      },
    },
  ],
  webServer: [
    {
      command: `${NETNS}go run ./cmd/graphite-meter`,
      cwd: "../go",
      url: `http://${HOST}:${ports.h1}/preflight`,
      env: serverEnv,
      reuseExistingServer: true,
      stdout: "pipe",
      timeout: 180_000,
    },
    {
      // dev, not preview: changing a constant is then a reload, not a rebuild.
      command: "bun run dev -- --host 127.0.0.1 --port 5173",
      url: "http://127.0.0.1:5173/bench/harness.html",
      reuseExistingServer: true,
      timeout: 120_000,
    },
  ],
});
