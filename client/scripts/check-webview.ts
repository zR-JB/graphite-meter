import { createChromeWebView } from "../browser/chrome";

const path = process.env.BUN_CHROME_PATH;
const expected = process.env.GM_EXPECTED_CHROME_VERSION;
if (!path) throw new Error("BUN_CHROME_PATH is required");
if (!expected) throw new Error("GM_EXPECTED_CHROME_VERSION is required");

const version = Bun.spawnSync([path, "--version"], {
  stdout: "pipe",
  stderr: "inherit",
});
if (version.exitCode !== 0)
  throw new Error(`Chrome version check exited with ${version.exitCode}`);
const actual = version.stdout.toString().trim();
const wanted = `Google Chrome for Testing ${expected}`;
if (actual !== wanted)
  throw new Error(`Chrome version mismatch: got ${actual}, want ${wanted}`);

const view = createChromeWebView();
try {
  await view.navigate("about:blank");
  const browser = await view.cdp<{ product: string }>("Browser.getVersion");
  if (browser.product !== `Chrome/${expected}`)
    throw new Error(
      `WebView Chrome version mismatch: got ${browser.product}, want Chrome/${expected}`,
    );
  console.log(`WebView launch passed: ${actual}`);
} finally {
  view.close();
  Bun.WebView.closeAll();
}
