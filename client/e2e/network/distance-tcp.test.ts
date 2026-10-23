import { distanceTests } from "./distance";
import { browser } from "../webview";

// Firefox can wedge its socket thread on an HTTP/2 download its worker abandons, until step 5 ends downloads on the
// server's clock.
distanceTests([
  ["Automatic", "auto"],
  ["HTTP/1.1", "protocol:http1"],
  ...(browser === "chrome" ? [["HTTP/2", "protocol:http2"] as const] : []),
]);
