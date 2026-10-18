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

// Only Chrome takes the run's certificate for QUIC (--origin-to-force-quic-on), and Firefox 157 can wedge a worker's
// abandoned HTTP/2 download on a slow runner (a Firefox networking bug).
for (const [name, at, throughputTarget, origin, transport, protocol] of paths)
  (protocol === "http/1.1" ? test : test.chrome)(
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
      if (datagrams) {
        expect(server.latencyTarget).toEqual({ origin: home.h3, transport: "webtransport" });
        expect(server.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
      }
    },
  );
