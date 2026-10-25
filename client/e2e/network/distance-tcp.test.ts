import { distanceTests } from "./distance";

distanceTests([
  ["Automatic", "auto"],
  ["HTTP/1.1", "protocol:http1"],
  ["HTTP/2", "protocol:http2"],
]);
