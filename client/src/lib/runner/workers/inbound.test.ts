import { expect, test } from "bun:test";
import { ROUTES } from "../paths";
import { requestHeaders, requestUrl, tokenMint } from "./inbound";
import { stubFetch } from "./test-helpers.testutil";
import { parseInMsg as fetchMessage } from "./fetch-worker";
import { parseInMsg as pingMessage } from "./ping-worker";
import { parseInMsg as sessionMessage } from "./wt-transfer-worker";

const HTTP = ["http:", "https:"];

test("a worker requests only an http(s) measurement route without credentials or fragment", () => {
  expect(
    requestUrl("https://meter.test/download?bytes=1", HTTP, [ROUTES.download]),
  ).toBe("https://meter.test/download?bytes=1");
  for (const url of [
    "javascript:alert(1)",
    "file:///etc/passwd",
    "https://meter.test/auth/logout",
    "https://meter.test/download/../auth/logout",
    "https://user:secret@meter.test/download",
    "https://meter.test/download#x",
    "/download",
    42,
  ])
    expect(() => requestUrl(url, HTTP, [ROUTES.download])).toThrow(
      "invalid request URL",
    );
});

test("headers, credentials and token mints keep their exact shapes", () => {
  expect(requestHeaders({ Authorization: "Bearer t" })).toEqual({
    Authorization: "Bearer t",
  });
  expect(() => requestHeaders({ Authorization: 1 })).toThrow("invalid header");
  expect(() => requestHeaders(["x"])).toThrow("invalid body");
  expect(
    tokenMint({ url: "https://meter.test/wt/session", credentials: "include" }),
  ).toEqual({
    url: "https://meter.test/wt/session",
    headers: undefined,
    credentials: "include",
  });
  expect(() =>
    tokenMint({ url: "https://meter.test/wt/session", credentials: "any" }),
  ).toThrow("invalid credentials mode");
  expect(() => tokenMint({ url: "https://evil.test/collect" })).toThrow(
    "invalid request URL",
  );
});

test("each worker accepts its owner's messages and refuses every other shape", () => {
  expect(
    fetchMessage({
      type: "start",
      dir: "down",
      url: "https://meter.test/download?bytes=1",
      streams: 4,
    }),
  ).toMatchObject({ type: "start", dir: "down", streams: 4 });
  expect(pingMessage({ type: "stop", cutoffEpochMs: 1 })).toEqual({
    type: "stop",
    cutoffEpochMs: 1,
  });
  expect(sessionMessage({ type: "measure", seq: 3 })).toEqual({
    type: "measure",
    seq: 3,
  });
  const refused: [(data: unknown) => unknown, unknown][] = [
    [fetchMessage, null],
    [fetchMessage, { type: "stop" }],
    [
      fetchMessage,
      { type: "start", dir: "up", url: "https://meter.test/download" },
    ],
    [fetchMessage, { type: "measure", seq: -1 }],
    [
      fetchMessage,
      {
        type: "start",
        dir: "down",
        url: "https://meter.test/download",
        streams: 0,
      },
    ],
    [
      pingMessage,
      {
        type: "start",
        transport: "websocket",
        url: "https://meter.test/ws/ping",
      },
    ],
    [pingMessage, { type: "measure", intervalMs: Number.NaN }],
    [
      sessionMessage,
      { type: "start", dir: "down", url: "http://meter.test/wt/download" },
    ],
  ];
  for (const [parse, data] of refused)
    expect(() => parse(data)).toThrow("worker message has an invalid");
});

test("a message that did not arrive through the owner's port is ignored", async () => {
  let fetched = 0;
  const restore = stubFetch((async () => {
    fetched++;
    return new Response(null, { status: 400 });
  }) as unknown as typeof fetch);
  const realm = "foreign-origin";
  try {
    await import(`./fetch-worker.ts?realm=${realm}`);
    const handler = globalThis.onmessage as (event: MessageEvent) => void;
    const data = {
      type: "start",
      dir: "down",
      url: "https://meter.test/download",
    };
    handler({ origin: "https://evil.test", data } as MessageEvent);
    await Bun.sleep(0);
    expect(fetched).toBe(0);
  } finally {
    restore();
    globalThis.onmessage = null;
  }
});
