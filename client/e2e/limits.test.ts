import { baseConfig, open, runButton, spawnPeer } from "./fleet";
import { expect, test } from "./webview";

test("a stage longer than a selected server admits blocks the start and names that server", async (page) => {
  const oslo = await spawnPeer("Oslo", { GM_MAX_STAGE_DURATION: "2s" });
  try {
    await open(page, oslo.server.url, {
      config: { duration: { ...baseConfig.duration, downloadMs: 3_000 } },
    });
    await expect(runButton(page, "Start test")).toHaveAttribute(
      "aria-disabled",
      "true",
    );
    await expect(page.locator(".gauge-footer")).toContainText(
      "Oslo allows stages up to 2 s; shorten the Download stage.",
    );
  } finally {
    oslo.kill();
  }
});
