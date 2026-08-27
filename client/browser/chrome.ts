export const chromeLaunchArgs = () => [
  "--hide-scrollbars",
  ...(process.env.BUN_CHROME_ARGS ?? "").split(/\s+/).filter(Boolean),
];

export function createChromeWebView(
  consoleHandler?: Bun.WebView.ConsoleCapture,
) {
  const path = process.env.BUN_CHROME_PATH;
  if (process.env.CI && !path)
    throw new Error("BUN_CHROME_PATH is required in CI");

  return new Bun.WebView({
    width: 1280,
    height: 720,
    backend: {
      type: "chrome",
      url: false,
      path,
      argv: chromeLaunchArgs(),
      stderr:
        process.env.CI || process.env.GM_WEBVIEW_DEBUG ? "inherit" : "ignore",
    },
    dataStore: "ephemeral",
    console: consoleHandler,
  });
}
