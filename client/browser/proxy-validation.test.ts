import { expect, openApp, test } from "./webview";
test("proxy discovery does not restart its own validation", async ({
  page,
}) => {
  await page.addInitScript(() => {
    localStorage.setItem(
      "graphite-meter:v1",
      JSON.stringify({
        config: {
          stages: {
            latency: false,
            download: true,
            upload: false,
            bidirectional: false,
          },
          skipLoadedLatencyWhenStageOff: true,
          transports: { throughputTarget: "auto", latencyTarget: "auto" },
        },
      }),
    );
  });
  let preflights = 0;
  let probes = 0;
  await page.route("**/preflight?*", async (route) => {
    preflights++;
    await route.fulfill({
      json: {
        server: { name: "proxy-test" },
        engineVersion: "test",
        generation: "proxy-generation",
        capabilities: {
          throughput: [
            { baseUrl: ".", transport: "fetch-stream", protocol: "negotiated" },
          ],
          latency: [],
        },
      },
    });
  });
  await page.route("**/probe?*", async (route) => {
    probes++;
    await route.fulfill({
      json: {
        clientIp: "127.0.0.1",
        clientIpVersion: 4,
        clientIpSource: "socket",
        protocolNegotiated: "http/1.1",
      },
    });
  });
  await openApp(page, "real");
  await expect.poll(() => probes).toBe(1);
  await page.waitForTimeout(500);
  expect(preflights).toBe(1);
  expect(probes).toBe(1);
});
