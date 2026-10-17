// Estimates the wire rate (docs/MEASUREMENTS.md) as payload throughput times a modeled protocol overhead at a
// 1,500 B MTU. The terminal client shares its multipliers (api/wirerate.testvectors.json).

type Framing = "http3-data" | "webtransport-stream" | "webtransport-datagram";
type WireTransport = "http1-clear" | "https-tls" | "http2" | "http3-quic";
type FactorKey = "application-framing" | "tls-records" | "ethernet" | "ip" | "transport";

/** A result keeps this model beside its headline, as history saves it. */
export interface WireModel {
  componentCount?: number;
  /** Holds one row per header layer with its share of the payload in percent, weighted by bytes over all components. */
  factors: { key: FactorKey; label: string; contributionPct: number }[];
  transport: WireTransport;
  transportSource: "detected" | "fallback";
  framing: Framing | null;
  mtuBytes: number;
  ipVersion: 4 | 6;
  ipVersionSource: "detected" | "fallback";
  totalMultiplier: number;
}

/** Describes a measured path by the browser-observed protocol (or the WebTransport kind) and the probe's IP family. */
export interface WirePath {
  protocol: "http1" | "http2" | "http3" | "negotiated" | undefined;
  secure: boolean;
  ipVersion: 4 | 6;
  transport: "fetch-stream" | "webtransport" | "webtransport-datagram";
}

const MTU = 1_500;
const ETHERNET = 38; // 14 MAC + 4 FCS + 8 preamble/SFD + 12 inter-frame gap
const TCP = 20;
const TCP_TIMESTAMPS = 12;
const UDP = 8;
const QUIC_SHORT_HEADER = 1 + 8 + 2 + 16; // flags, typical connection ID and packet number, AEAD tag
const RECORD = 16_384; // TLS records and HTTP/2 and HTTP/3 DATA frames at 16 KiB
const TLS_RECORD = 22; // 5 record + 1 inner type + 16-byte AEAD tag
const HTTP2_FRAME = 9;
const HTTP3_FRAME = 5; // a one-byte type and a four-byte length at a 16 KiB frame

const FRAMING_LABEL: Record<Framing, string> = {
  "http3-data": "HTTP/3 DATA frames",
  "webtransport-stream": "WebTransport QUIC stream frames",
  "webtransport-datagram": "WebTransport QUIC datagrams",
};

/** Models one path. Its factors are unweighted. */
export function pathModel(path: WirePath): WireModel {
  const framing: Framing | null =
    path.transport === "webtransport-datagram"
      ? "webtransport-datagram"
      : path.transport === "webtransport"
        ? "webtransport-stream"
        : path.protocol === "http3"
          ? "http3-data"
          : null;
  const transport: WireTransport = framing
    ? "http3-quic"
    : path.protocol === "http2"
      ? "http2"
      : path.secure
        ? "https-tls"
        : "http1-clear";
  const factors: WireModel["factors"] = [];
  let application = 1;
  const wrap = (key: FactorKey, label: string, ratio: number) => {
    factors.push({ key, label, contributionPct: application * ratio * 100 });
    application *= 1 + ratio;
  };
  if (transport === "http2") wrap("application-framing", "HTTP/2 DATA frames", HTTP2_FRAME / RECORD);
  if (framing) wrap("application-framing", FRAMING_LABEL[framing], HTTP3_FRAME / RECORD);
  if (transport === "https-tls" || transport === "http2") wrap("tls-records", "TLS 1.3 records", TLS_RECORD / RECORD);
  const ip = path.ipVersion === 6 ? 40 : 20;
  const header = framing ? UDP + QUIC_SHORT_HEADER : TCP + TCP_TIMESTAMPS;
  const payload = MTU - ip - header;
  const layer = (key: FactorKey, label: string, bytes: number) =>
    factors.push({ key, label, contributionPct: application * (bytes / payload) * 100 });
  layer("ethernet", "Ethernet", ETHERNET);
  layer("ip", path.ipVersion === 6 ? "IPv6" : "IPv4", ip);
  layer("transport", framing ? "UDP + QUIC" : "TCP + options", header);
  return {
    factors,
    transport,
    transportSource: framing || (path.protocol && path.protocol !== "negotiated") ? "detected" : "fallback",
    framing,
    mtuBytes: MTU,
    ipVersion: path.ipVersion,
    ipVersionSource: "detected",
    totalMultiplier: (application * (MTU + ETHERNET)) / payload,
  };
}

/** Combines several components (servers, or both directions) into one model, each weighted by its measured bytes. */
export function combinedModel(components: readonly { bytesPerSec: number; path: WirePath }[]): WireModel | null {
  const measured = components.reduce((sum, { bytesPerSec }) => sum + bytesPerSec, 0);
  if (!components.length) return null;
  const models = components.map(({ path }) => pathModel(path));
  const rows = new Map<string, WireModel["factors"][number]>();
  let wire = 0;
  components.forEach(({ bytesPerSec }, index) => {
    const model = models[index]!;
    wire += bytesPerSec * model.totalMultiplier;
    if (!(bytesPerSec > 0) || !(measured > 0)) return;
    for (const factor of model.factors) {
      const row = rows.get(factor.label) ?? { ...factor, contributionPct: 0 };
      row.contributionPct += (factor.contributionPct * bytesPerSec) / measured;
      rows.set(factor.label, row);
    }
  });
  const first = models[components.findIndex(({ bytesPerSec }) => bytesPerSec > 0)] ?? models[0]!;
  return {
    ...first,
    componentCount: components.length,
    factors: [...rows.values()],
    totalMultiplier: measured > 0 ? wire / measured : 1,
  };
}
