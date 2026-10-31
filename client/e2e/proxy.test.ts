import { createServer, request } from "node:http";
import { connect } from "node:net";
import { baseConfig, open, ready, run, spawnPeer } from "./fleet";
import { expect, test } from "./webview";

test("a buffered progress feed keeps the upload running through an HTTP proxy", async (page) => {
  let upstream: URL;
  let uploads = 0;
  let feeds = 0;
  let withheld = 0;
  let checkpoints = 0;
  const proxy = createServer((incoming, outgoing) => {
    const path = incoming.url!;
    if (incoming.method === "POST" && (path === "/upload" || path.startsWith("/upload?")))
      uploads++;
    const feed = incoming.method === "GET" && path.startsWith("/upload/progress?");
    if (feed) feeds++;
    if (path.startsWith("/upload/checkpoint?")) checkpoints++;
    const forwarded = request(
      {
        hostname: upstream.hostname,
        port: upstream.port,
        method: incoming.method,
        path,
        headers: incoming.headers,
      },
      (response) => {
        outgoing.writeHead(response.statusCode!, response.headers);
        if (feed) {
          // Model a proxy buffering the streaming response while leaving the request open.
          outgoing.flushHeaders();
          response.resume();
          response.on("end", () => outgoing.end());
        } else response.pipe(outgoing);
      },
    );
    forwarded.on("error", () => outgoing.destroy());
    incoming.pipe(forwarded);
  });
  proxy.on("upgrade", (incoming, socket, head) => {
    const feed = incoming.url!.startsWith("/ws/progress?");
    if (feed) feeds++;
    const target = connect(Number(upstream.port), upstream.hostname, () => {
      const headers = incoming.rawHeaders.reduce(
        (text, value, index, all) => (index % 2 ? text : `${text}${value}: ${all[index + 1]}\r\n`),
        "",
      );
      target.write(
        `${incoming.method} ${incoming.url} HTTP/${incoming.httpVersion}\r\n${headers}\r\n`,
      );
      target.write(head);
      socket.pipe(target);
      if (feed) {
        let buffered = Buffer.alloc(0);
        let headerEnd = -1;
        let readyForwarded = false;
        target.on("data", (chunk: Buffer) => {
          if (readyForwarded) {
            withheld += chunk.length;
            return;
          }
          buffered = Buffer.concat([buffered, chunk]);
          if (headerEnd < 0) {
            const end = buffered.indexOf("\r\n\r\n");
            if (end < 0) return;
            headerEnd = end + 4;
            socket.write(buffered.subarray(0, headerEnd));
          }
          const frame = buffered.subarray(headerEnd);
          if (frame.length < 2) return;
          const marker = frame[1]! & 0x7f;
          const lengthBytes = marker === 126 ? 2 : marker === 127 ? 8 : 0;
          const headerBytes = 2 + lengthBytes + (frame[1]! & 0x80 ? 4 : 0);
          if (frame.length < headerBytes) return;
          const length =
            lengthBytes === 2
              ? frame.readUInt16BE(2)
              : lengthBytes === 8
                ? Number(frame.readBigUInt64BE(2))
                : marker;
          const frameEnd = headerBytes + length;
          if (frame.length < frameEnd) return;
          socket.write(frame.subarray(0, frameEnd));
          withheld += frame.length - frameEnd;
          buffered = Buffer.alloc(0);
          readyForwarded = true;
        });
      } else target.pipe(socket);
    });
    target.on("error", () => socket.destroy());
    socket.on("error", () => target.destroy());
  });
  await new Promise<void>((resolve) => proxy.listen(0, "127.0.0.1", resolve));
  const origin = `http://127.0.0.1:${(proxy.address() as { port: number }).port}`;
  let peer: Awaited<ReturnType<typeof spawnPeer>> | undefined;
  try {
    peer = await spawnPeer("Proxied", {
      GM_H1_PUBLIC_ORIGIN: origin,
      GM_ADVERTISED_NATIVE_ENDPOINTS: "http1-clear",
    });
    upstream = new URL(peer.server.http);
    await open(page, origin, {
      servers: [{ id: "self", url: origin }],
      config: {
        ...baseConfig,
        duration: { ...baseConfig.duration, uploadMs: 4000 },
        transports: { throughputTarget: "protocol:http1", latencyTarget: "transport:websocket" },
      },
    });
    await ready(page);
    const saved = await run(page);
    expect(saved.result.outcome).toBe("complete");
    expect(saved.result.multiServer.failures).toEqual([]);
    expect(saved.result.multiServer.servers[0]!.throughput.origin).toBe(origin);
    expect(saved.result.multiServer.servers[0]!.upload!.reportedBytesPerSec).toBeGreaterThan(0);
    expect(uploads).toBeGreaterThan(0);
    expect(checkpoints).toBeGreaterThan(5);
    expect(feeds).toBe(1);
    expect(withheld).toBeGreaterThan(0);
  } finally {
    peer?.kill();
    proxy.closeAllConnections();
    proxy.close();
  }
});
