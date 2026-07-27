// End to end: a real browser, the production lanes, and a real server. The
// stubbed browser suite beside it (./browser) never reaches a backend, so this
// is the only place both ends are real.
//
// Chromium only. QUIC ignores ignoreHTTPSErrors, and Firefox reaches h3 only
// through a system trust anchor, which is more than a check of this size should
// install.
import { defineConfig } from "@playwright/test";

const HOST = "127.0.0.1";
// Off the 7246-7249 convention on purpose: a live run must not fight, or
// silently reuse, a `just dev` server already on those ports.
const ports = { h1: 7256, h1tls: 7257, h3: 7259 };

/** Set by the recipe from the throwaway certificate it generates. Absent means
 *  the recipe was bypassed; failing here beats a QUIC error 40 minutes later. */
const SPKI = process.env.GM_E2E_SPKI;
if (!SPKI) throw new Error("GM_E2E_SPKI unset: run `just client-e2e`");

export const origins = {
  "h1-clear": `http://${HOST}:${ports.h1}`,
  h3: `https://${HOST}:${ports.h3}`,
};

export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/*.pw.ts",
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "github" : "list",
  timeout: 60_000,
  use: {
    baseURL: `http://${HOST}:5273`,
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: {
        browserName: "chromium",
        launchOptions: {
          args: [
            // Trust the throwaway leaf by hash, and skip the Alt-Svc bootstrap
            // race, which is the flake this check would otherwise carry.
            `--ignore-certificate-errors-spki-list=${SPKI}`,
            `--origin-to-force-quic-on=${HOST}:${ports.h3}`,
          ],
        },
      },
    },
  ],
  webServer: [
    {
      command: "go run ./cmd/graphite-meter",
      cwd: "../go",
      url: `http://${HOST}:${ports.h1}/preflight`,
      env: {
        GM_H1_ADDR: `${HOST}:${ports.h1}`,
        GM_H1_TLS_ADDR: `${HOST}:${ports.h1tls}`,
        GM_H3_ADDR: `${HOST}:${ports.h3}`,
        GM_TLS_CERT: process.env.GM_E2E_TLS_CERT ?? "",
        GM_TLS_KEY: process.env.GM_E2E_TLS_KEY ?? "",
      },
      reuseExistingServer: false,
      stdout: "pipe",
      timeout: 180_000,
    },
    {
      command: `bun run dev -- --host ${HOST} --port 5273`,
      url: `http://${HOST}:5273/bench/harness.html`,
      reuseExistingServer: false,
      timeout: 120_000,
    },
  ],
});
