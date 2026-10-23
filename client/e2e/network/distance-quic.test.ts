import { distanceTests } from "./distance";
import { browser } from "../webview";

const quota =
  browser === "firefox"
    ? "until step 3: Firefox's own QUIC connections fill the server's per-address quota"
    : undefined;

distanceTests([
  ["HTTP/3", "protocol:http3", quota],
  ["WebTransport", "transport:webtransport", quota],
]);
