// The explanations Settings and Details show beside their words: a title line, then a few short lines.
import type { PingCadence, Role } from "../../engine/types";

export const TIP = {
  testServers: "Test servers\nUp to 4; their speeds add up\nAll are probed; the lanes show one at a time",
  stageTime: "Duration\nEach stage's planned length\nUp to the servers' limit, 5 min by default",
  warmup: "Warmup\nOpens connections before each stage\n10 round trips to 4 s; never counted",
  bidirectionalStage: "Bidirectional stage\nDownload and upload at once, after the others",
  earlyFinish: "Early finish\nEnds a steady stage after about half its time\nNever after a stall, gap or lost server",
  units:
    "Units\nBits: Mbit/s, as internet plans\nBytes: MB/s, 8× lower, as downloads\n" +
    "Decimal: steps of 1,000 (k, M, G)\nBinary: steps of 1,024 (Ki, Mi, Gi)",
  explanations:
    "Explanations\nOn hover: as the pointer rests on a word or figure\nOn click: only when it is clicked or tapped",
  wireRate: "Wire rate\nPayload plus the protocol headers carried with it\nAn estimate; hover a result for its parts",
  keyShortcuts:
    "Keyboard shortcuts\nS, D, H, R and T act when no field has focus\nTurn off if speech input triggers them",
  gaugeAuto: "Automatic scale\nThe chart follows the peak\nThe gauge steps 1, 2, 5, 10 of its unit",
  gaugeMax: "Maximum\nFixes the chart ceiling\nThe gauge rounds up to a 1, 2, 5 step",
  saveResults: "Save results\nKeeps the newest 2,000 runs in this browser\nNothing is uploaded",
  idleCadence:
    "Idle latency cadence\nReply: the next probe leaves on each reply\nFast 80 ms · Medium 250 ms · Slow 600 ms",
  loadedCadence:
    "Loaded latency cadence\nProbes during transfers, start to start\nFast 80 ms · Medium 250 ms · Slow 600 ms",
  skipLoadedLatency: "Skip loaded latency\nWith the Latency stage off, transfers send no probes",
  forcedStreams:
    "Streams\nParallel connections per server and direction\nForced opens the exact count, past browser limits",
  forcedStreamCount:
    "Streams per server and direction\nOpens exactly this many\nWebTransport carries at most 16 per session",
  autoStreamCount:
    "HTTP/1.1 stream limit\nParallel HTTP/1.1 requests per direction\nHTTP/2 and HTTP/3 choose their own",
  datagramThroughput:
    "Datagram throughput\nAdds WebTransport datagrams as a path\nNever resent; expect lower rates than streams",
  pathEvidence: "Evidence\nThe HTTP version each side observed",
  uploadFeed: "Upload feed\nHow the server reports upload bytes back",
  clientAddress: "Your address\nAs the server saw it: proxy header or socket\nHidden until you press the eye beside it",
  serverLoad: "Load\nTests on the server when the path was checked\nPast half its slots, they share the bandwidth",
  serverInstance: "Server\nEngine version; the instance changes on restart",
  pretestLatency: "Pre-test latency\nMedian round trip of the path check\nThe shown server's sizes the warmup",
} as const;

export const PATH_TIP: Record<Role, string> = {
  throughput: "Throughput path\nTransport and HTTP version of the test bytes",
  latency: "Latency path\nTransport of the probes, on its own connection",
};

/** A path choice's second line: what it does, in a few words. */
export const PATH_NOTE: Record<Role, Record<string, string>> = {
  throughput: {
    auto: "HTTP/1.1, else HTTP/2, HTTP/3 or WebTransport",
    "protocol:http1": "Parallel connections, one stream each",
    "protocol:http2": "One connection, several streams",
    "protocol:http3": "One QUIC connection, several streams",
    "transport:webtransport": "Streams in one HTTP/3 session",
    "transport:webtransport-datagram": "Experimental unreliable datagrams",
  },
  latency: {
    auto: "WebTransport datagrams, else WebSocket",
    "transport:websocket": "Reliable messages over one connection",
    "transport:webtransport": "Unreliable messages in one HTTP/3 session",
  },
};

/** A cadence's name, and its word on a segment. */
export const CADENCE: Record<PingCadence, { label: string; short: string }> = {
  "reply-driven": { label: "Reply-driven", short: "Reply" },
  fast: { label: "Fast (80 ms)", short: "Fast" },
  medium: { label: "Medium (250 ms)", short: "Medium" },
  slow: { label: "Slow (600 ms)", short: "Slow" },
};

export const preflightNote = (ms: string) =>
  `Preflight request ${ms} ms: connection setup plus the response\nNot a latency measurement`;
