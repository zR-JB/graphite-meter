import { baseConfig, home, open, ready, run } from "./fleet";
import { expect, test } from "./webview";

const PROTOCOL = { "http/1.1": "http1", h2: "http2", h3: "http3" } as const;
const paths = [
  ["HTTP/1.1 clear", home.http, "protocol:http1", home.http, "fetch-stream", "http/1.1"],
  ["HTTP/1.1 TLS", home.url, "protocol:http1", home.url, "fetch-stream", "http/1.1"],
  ["HTTP/2", home.url, "protocol:http2", home.h2, "fetch-stream", "h2"],
  ["HTTP/3", home.url, "protocol:http3", home.h3, "fetch-stream", "h3"],
  ["WebTransport streams", home.url, "transport:webtransport", home.h3, "webtransport", "h3"],
  [
    "WebTransport datagrams",
    home.url,
    "transport:webtransport-datagram",
    home.h3,
    "webtransport-datagram",
    "h3",
  ],
] as const;

// Only Chrome takes the run's certificate for QUIC. These stages run their planned time, so downloads end on the server's
// clock and no worker ends mid-download.
for (const [name, at, throughputTarget, origin, transport, protocol] of paths)
  (protocol === "h3" ? test.chrome : test)(
    `${name} carries both directions over ${protocol}`,
    async (page) => {
      // Latency's WebTransport path is the datagram bus too, so this cell carries both.
      const datagrams = transport === "webtransport-datagram";
      await open(page, at, {
        config: {
          transports: {
            throughputTarget,
            latencyTarget: datagrams ? "transport:webtransport" : "transport:websocket",
          },
          stages: { ...baseConfig.stages, latency: datagrams },
        },
      });
      await ready(page);
      await page.evaluate(() => {
        const feeds = new Set<string>();
        const fetch = window.fetch.bind(window);
        window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
          const url = new URL(String(input), location.href);
          if (url.pathname === "/upload/progress" && !init?.method)
            feeds.add(url.origin + url.pathname);
          return fetch(input, init);
        }) as typeof fetch;
        window.WebSocket = class extends WebSocket {
          constructor(at: string | URL, protocols?: string | string[]) {
            super(at, protocols);
            const url = new URL(at);
            if (url.pathname === "/ws/progress") feeds.add(url.origin + url.pathname);
          }
        };
        Object.assign(window, { feeds });
      });
      const saved = await run(page);
      expect(saved.result.outcome).toBe("complete");
      expect(saved.result.multiServer.failures).toEqual([]);
      const server = saved.result.multiServer.servers[0]!;
      expect(server.throughput).toMatchObject({
        origin,
        transport,
        protocol: PROTOCOL[protocol],
        browserProtocol: protocol,
      });
      expect(server.download?.reportedBytesPerSec).toBeGreaterThan(0);
      expect(server.upload?.reportedBytesPerSec).toBeGreaterThan(0);
      // An upload's feed goes to the discovery origin, the page's here, over its WebSocket as it serves HTTP/1.1.
      const feed = `${at.replace(/^http/, "ws")}/ws/progress`;
      expect(await page.evaluate(() => [...(window as any).feeds])).toEqual(
        transport === "fetch-stream" ? [feed] : [],
      );
      if (datagrams) {
        expect(server.latencyTarget).toEqual({ origin: home.h3, transport: "webtransport" });
        expect(server.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
      }
    },
  );
