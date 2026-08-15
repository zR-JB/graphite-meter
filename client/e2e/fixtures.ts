import { test as base, expect } from "@playwright/test";
import { createReadStream } from "node:fs";
import { stat } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { dirname, extname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const HOST = "127.0.0.1";
const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../.e2e-dist");

const contentTypes: Record<string, string> = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
};

function insideRoot(path: string): boolean {
  const rel = relative(root, path);
  return rel === "" || (!rel.startsWith(`..${sep}`) && rel !== "..");
}

function closeServer(server: Server): Promise<void> {
  return new Promise((resolveClose, rejectClose) => {
    server.close((error) => (error ? rejectClose(error) : resolveClose()));
  });
}

async function startHarnessServer(): Promise<{ server: Server; origin: string }> {
  const server = createServer(async (request, response) => {
    try {
      const requestURL = new URL(request.url ?? "/", `http://${HOST}`);
      const decoded = decodeURIComponent(requestURL.pathname);
      const path = resolve(root, `.${decoded}`);
      if (!insideRoot(path)) {
        response.writeHead(403).end("forbidden\n");
        return;
      }
      const info = await stat(path);
      if (!info.isFile()) {
        response.writeHead(404).end("not found\n");
        return;
      }
      response.writeHead(200, {
        "Content-Type": contentTypes[extname(path)] ?? "application/octet-stream",
        "Cache-Control": "no-store",
      });
      createReadStream(path).pipe(response);
    } catch {
      response.writeHead(404).end("not found\n");
    }
  });

  await new Promise<void>((resolveListen, rejectListen) => {
    const onError = (error: Error): void => rejectListen(error);
    server.once("error", onError);
    server.listen(0, HOST, () => {
      server.off("error", onError);
      resolveListen();
    });
  });
  const address = server.address();
  if (address === null || typeof address === "string") {
    await closeServer(server);
    throw new Error("E2E harness server did not expose a TCP address");
  }
  return { server, origin: `http://${HOST}:${(address as AddressInfo).port}` };
}

type WorkerFixtures = {
  harnessOrigin: string;
};

export const test = base.extend<{}, WorkerFixtures>({
  harnessOrigin: [
    async ({}, use) => {
      const { server, origin } = await startHarnessServer();
      try {
        await use(origin);
      } finally {
        await closeServer(server);
      }
    },
    { scope: "worker" },
  ],
});

export { expect };
