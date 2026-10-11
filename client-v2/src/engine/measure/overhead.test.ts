import { expect, test } from "bun:test";
import { json } from "../../protocol/pins.testutil";
import { combinedModel, pathModel } from "./overhead";

test("the wire multipliers the terminal clients share", async () => {
  for (const { scheme, transport, protocol, ipVersion, multiplier } of (await json("wirerate.testvectors.json")).cases)
    expect(pathModel({ protocol, secure: scheme === "https", ipVersion, transport }).totalMultiplier).toBeCloseTo(
      multiplier,
      9,
    );
});

test("servers on different paths weigh their overhead by their bytes", () => {
  const http1 = { protocol: "http1", secure: false, ipVersion: 4, transport: "fetch-stream" } as const;
  const quic = { protocol: "http3", secure: true, ipVersion: 6, transport: "fetch-stream" } as const;
  const model = combinedModel([
    { bytesPerSec: 3e8, path: http1 },
    { bytesPerSec: 1e8, path: quic },
  ])!;
  const expected = (3 * pathModel(http1).totalMultiplier + pathModel(quic).totalMultiplier) / 4;
  expect(model.totalMultiplier).toBeCloseTo(expected, 12);
  expect(model.componentCount).toBe(2);
  const share = (label: string) => model.factors.find((factor) => factor.label === label)?.contributionPct ?? 0;
  expect(
    share("IPv4") +
      share("IPv6") +
      share("Ethernet") +
      share("TCP + options") +
      share("UDP + QUIC") +
      share("HTTP/3 DATA frames") +
      share("TLS 1.3 records"),
  ).toBeCloseTo((model.totalMultiplier - 1) * 100, 9);
});
