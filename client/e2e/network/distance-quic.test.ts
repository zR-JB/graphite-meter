import { distanceTests } from "./distance";

distanceTests([
  ["HTTP/3", "protocol:http3"],
  ["4-stream HTTP/3", "protocol:http3", { transferStreams: { mode: "forced", count: 4 } }],
  ["WebTransport", "transport:webtransport"],
]);
