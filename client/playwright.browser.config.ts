// The stubbed browser suite: accessibility, panel behaviour, presentation.
// It serves the bundle alone, so nothing here reaches a backend — every
// transport is stubbed or synthetic. The real ones live in ./e2e.
import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./browser",
  testMatch: "**/*.pw.ts",
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "github" : "list",
  use: {
    baseURL: "http://127.0.0.1:4173",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: {
        browserName: "chromium",
        launchOptions: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
          ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH }
          : undefined,
      },
    },
    { name: "firefox", use: { browserName: "firefox" } },
  ],
  // The calling browser-test recipe builds the bundle, so this timeout covers only the
  // preview server binding.
  webServer: {
    command: "bun run preview -- --host 127.0.0.1 --port 4173",
    url: "http://127.0.0.1:4173",
    reuseExistingServer: false,
    timeout: 30_000,
  },
});
