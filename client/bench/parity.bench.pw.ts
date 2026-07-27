// App-parity: the shipped app, driven end to end against the same server the
// harness uses. If the two disagree, the harness measures something the product
// does not, and every other number here is provisional.
import { test, expect } from "@playwright/test";

// The server's own origin: it serves the built SPA on the h1 clear listener, so
// the app and the transfers it measures share a host, as a real deployment does.
const APP = process.env.GM_BENCH_APP ?? "http://127.0.0.1:7246";
const STORAGE_KEY = "graphite-meter:v1";

// A benchmark needs a fixed window, so the adaptive early finish is off and
// every stage but download is disabled.
const config = {
  stages: {
    latency: false,
    download: true,
    upload: false,
    bidirectional: false,
  },
  duration: {
    warmupMs: 3000,
    latencyMs: 1000,
    downloadMs: 8000,
    uploadMs: 8000,
    bidirectionalMs: 8000,
  },
  adaptive: { enabled: false },
  transferStreams: { mode: "forced", count: 4 },
};

test("app-parity: the shipped app's own download number", async ({
  page,
  browserName,
}) => {
  page.on("pageerror", (e) => console.log(`PAGEERROR: ${e.message}`));
  page.on("console", (m) => {
    if (m.type() === "error") console.log(`CONSOLE: ${m.text()}`);
  });
  await page.addInitScript(
    ([key, value]) => window.localStorage.setItem(key, value),
    [STORAGE_KEY, JSON.stringify(config)] as const,
  );
  await page.goto(APP);
  await page.getByRole("button", { name: "Start the speed test" }).click();
  await expect(
    page.getByRole("button", { name: "Run the test again" }),
  ).toBeVisible({ timeout: 120_000 });

  // The rendered result is the app's own claim, which is the point of comparing.
  const text = await page.locator("body").innerText();
  console.log(`----- ${browserName} BODY -----\n${text.slice(0, 1500)}\n-----`);
  expect(text.length).toBeGreaterThan(0);
});
