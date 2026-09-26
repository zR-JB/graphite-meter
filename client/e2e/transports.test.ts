import type { CellResult, CellSpec } from "../bench/harness";
import { harness, home } from "./fleet";
import { expect, test } from "./webview";

const paths = [
  ["HTTP/1.1 clear", home.http, "fetch-stream", "http/1.1"],
  ["HTTP/1.1 TLS", home.url, "fetch-stream", "http/1.1"],
  ["HTTP/2", home.h2, "fetch-stream", "h2"],
  ["HTTP/3", home.h3, "fetch-stream", "h3"],
  ["WebTransport streams", home.h3, "webtransport", "h3"],
  ["WebTransport datagrams", home.h3, "webtransport-datagram", "h3"],
] as const;

for (const [name, origin, transport, protocol] of paths)
  test(`${name} carries both directions over ${protocol}`, async (page) => {
    await page.goto(`${harness}/bench/harness.html`);
    for (const dir of ["down", "up"] as const) {
      const spec: CellSpec = {
        origin,
        transport,
        dir,
        lanes: 1,
        warmupMs: 250,
        measureMs: 500,
      };
      const result = await page.evaluate<CellResult>(
        (cell) => (window as any).__gmBench.run(cell),
        spec,
      );
      expect(result.errors).toEqual([]);
      expect(result.bytes).toBeGreaterThan(0);
    }
    const negotiated = await page.evaluate(
      (base) =>
        fetch(`${base}/probe`, { cache: "no-store" })
          .then((response) => response.json())
          .then((probe) => probe.protocolNegotiated),
      origin,
    );
    expect(negotiated).toBe(protocol);
  });
