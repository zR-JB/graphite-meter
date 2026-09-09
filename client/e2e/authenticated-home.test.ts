import { fleet, fixturePassword, test, expect } from "./multi-server-fixtures";
import { startTest, waitForCompletion } from "../browser/webview";
import { configure, savedResult, ready } from "./multi-server-actions";

for (const transport of [
  "auto",
  "protocol:http2",
  "protocol:http3",
  "transport:webtransport",
  "transport:webtransport-datagram",
] as const)
  test(`authenticated home in a catalogue completes self-only with ${transport}`, async ({
    page,
  }) => {
    await page.goto(`${fleet[4].url}/login`);
    await page.getByLabel("Operator password").fill(fixturePassword);
    await page
      .getByRole("button", { name: "Sign in with operator password" })
      .click();
    await expect(
      page.getByRole("button", { name: "Open settings" }),
    ).toBeVisible();
    await configure(
      page,
      ["self"],
      1500,
      {
        experimentalDatagramThroughput:
          transport === "transport:webtransport-datagram",
        transports: {
          throughputTarget: transport,
          latencyTarget: "transport:websocket",
        },
      },
      { mode: "primary", serverId: "self" },
      fleet[4].url,
    );
    await ready(page);
    const startedAt = Date.now();
    await startTest(page);
    await waitForCompletion(page, 30000);
    const saved = await savedResult(page, startedAt);
    expect(saved.multiServer?.selection).toHaveLength(1);
    expect(saved.multiServer?.participants).toEqual(["self"]);
    expect(saved.multiServer?.intervals).toEqual([]);
    expect(saved.multiServer?.failures).toEqual([]);
    expect(saved.stages.download.result?.reportedBytesPerSec).toBeGreaterThan(
      0,
    );
    expect(saved.stages.upload.result?.reportedBytesPerSec).toBeGreaterThan(0);
    await page.raw.cdp("Network.clearBrowserCookies");
  });
