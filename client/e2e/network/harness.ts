// The network group's tools: a shaped link per server and direction, UDP outages, a same-address neighbour, and each
// server's connection log, from which a run's budget is counted. `mise run e2e <server> network` runs the group in a
// private network namespace, or on a CI runner's own loopback, whose queues its tests own one at a time.
import { test } from "bun:test";
import { readFileSync } from "node:fs";
import type { HistoryRecord } from "../../src/app/history/records";
import type { TestConfig } from "../../src/engine/types";
import {
  baseConfig,
  catalog,
  HISTORY_DB,
  open,
  password,
  passwordHash,
  readLatest,
  ready,
  runButton,
} from "../fleet";
import { freshBrowser } from "../browsers";
import { launch, NETWORK_FLEET, type Server } from "../servers";
import { browser, expect, type Page } from "../webview";

/** One direction of a server's access link: one-way delay, rate and random loss, behind a 1000-packet queue. */
export interface Direction {
  delayMs: number;
  mbit: number;
  lossPercent?: number;
}
export interface Link {
  down: Direction;
  up: Direction;
}
export const link = (rttMs: number, mbit: number): Link => ({
  down: { delayMs: rttMs / 2, mbit },
  up: { delayMs: rttMs / 2, mbit },
});

/** A run's settings: throughput over `throughputTarget`, latency Automatic, short stages and automatic streams. */
export const runConfig = (
  throughputTarget: string,
  patch: Partial<TestConfig> = {},
): Partial<TestConfig> => ({
  transports: { throughputTarget, latencyTarget: "auto" },
  stages: { latency: true, download: true, upload: true, bidirectional: false },
  duration: {
    warmupMs: 800,
    latencyMs: 2000,
    downloadMs: 3000,
    uploadMs: 3000,
    bidirectionalMs: 3000,
  },
  transferStreams: { mode: "auto", count: 4 },
  ...patch,
});

/** Server settings with `handlers` handlers in all, few enough for another client to hold every one for minutes. */
export const capped = (handlers: number) => ({
  GM_MAX_ACTIVE_MEASUREMENTS: String(handlers),
  GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT: String(handlers),
  GM_MAX_ACTIVE_SESSIONS: String(handlers),
  GM_MAX_OPERATION_DURATION: "5m",
  GM_MAX_SESSION_DURATION: "5m",
});

const address = (server: Server) => new URL(server.url).hostname;

// Root in the group's own network namespace; on a CI runner's loopback, sudo, which never prompts.
const TC = process.env.GM_E2E_HOST_LOOPBACK ? ["sudo", "-n", "tc"] : ["tc"];

function tc(command: string) {
  const run = Bun.spawnSync([...TC, ...command.split(" ")], { stderr: "pipe" });
  if (run.exitCode !== 0) throw new Error(`tc ${command}: ${run.stderr}`);
}

// Unmatched traffic, the shared fleet's included, takes band 1 as it is; server i's download takes 4 + 2i, its upload 5 + 2i.
function shape(links: Link[]) {
  // Loopback's queues are the group's alone, so whatever a failed test left is cleared first.
  for (const leftover of ["root", "ingress"])
    Bun.spawnSync([...TC, "qdisc", "del", "dev", "lo", leftover], { stderr: "ignore" });
  tc(
    `qdisc add dev lo root handle 1: prio bands ${3 + 2 * links.length} priomap ${"0 ".repeat(16).trim()}`,
  );
  tc("qdisc add dev lo handle ffff: ingress");
  links.forEach((link, i) => {
    const sides = [
      ["src", 4 + 2 * i, link.down],
      ["dst", 5 + 2 * i, link.up],
    ] as const;
    for (const [side, band, { delayMs, mbit, lossPercent = 0 }] of sides) {
      // tc reads class and handle numbers as hexadecimal.
      const minor = band.toString(16);
      tc(
        `qdisc add dev lo parent 1:${minor} handle ${minor}0: netem delay ${delayMs}ms loss ${lossPercent}% rate ${mbit}mbit limit 1000`,
      );
      tc(
        `filter add dev lo parent 1: protocol ip prio 1 u32 match ip ${side} ${address(NETWORK_FLEET[i]!)}/32 flowid 1:${minor}`,
      );
    }
  });
}

/** One server's GM_VERBOSE connection log; a mark is a byte offset, so a window holds what a run caused. */
export class ConnectionLog {
  constructor(readonly path: string) {}
  mark() {
    return Bun.file(this.path).size;
  }
  lines(from = 0, to = Infinity) {
    const text = readFileSync(this.path);
    return text.subarray(from, Math.min(to, text.length)).toString().split("\n");
  }
}

export type Fleet = Awaited<ReturnType<typeof shapedFleet>>;
// A test that timed out runs on beside the next, so the next ends its fleet before taking the same addresses.
let current: { close(): Promise<void> } | undefined;

/** Servers S1.. on links of their own, the first with the others in its catalog and, with `signIn`, a password. */
export async function shapedFleet(
  links: Link[],
  { signIn = false, env = [] }: { signIn?: boolean; env?: Record<string, string>[] } = {},
) {
  await current?.close();
  const owner = (current = { close: async () => {} });
  const servers = NETWORK_FLEET.slice(0, links.length);
  // The servers' origins outlive each test, and a browser keeps what it learned of them, such as Firefox's HTTP/3
  // exclusions, for as long as it runs.
  await freshBrowser();
  shape(links);
  const launched = JSON.parse(process.env.GM_E2E_LAUNCH!);
  const launches = await Promise.allSettled(
    servers.map((server, i) =>
      launch(
        launched,
        server,
        {
          GM_VERBOSE: "true",
          GM_ADVERTISED_NATIVE_ENDPOINTS: "http1-tls,http2,http3",
          ...(i === 0 && servers.length > 1 && catalog(...servers.slice(1))),
          ...(i === 0 &&
            signIn && {
              GM_AUTH_MODE: "password",
              GM_AUTH_PUBLIC_URL: server.url,
              GM_AUTH_PASSWORD_HASH: passwordHash,
            }),
          ...env[i],
        },
        false,
      ),
    ),
  );
  const children = launches.flatMap((launch) =>
    launch.status === "fulfilled" ? [launch.value] : [],
  );
  const neighbours: Bun.Subprocess[] = [];
  let closing: Promise<void> | undefined;
  const fleet = {
    servers,
    signIn,
    children,
    logs: children.map((child) => new ConnectionLog(child.log)),
    /** Drops every UDP packet to and from `server` for `ms`. */
    async udpOutage(server: Server, ms: number) {
      const match = `protocol ip prio 2 u32 match ip protocol 17 0xff match ip`;
      tc(`filter add dev lo parent ffff: ${match} dst ${address(server)}/32 action drop`);
      tc(`filter add dev lo parent ffff: ${match} src ${address(server)}/32 action drop`);
      await Bun.sleep(ms);
      tc("filter del dev lo parent ffff: prio 2");
    },
    /**
     * Another client holding `requests` requests to `url` at once, each asked again `everyMs` after its answer, read
     * slowly, until `stopNeighbours`. Over HTTP/3 it shares the browser's address, as a device behind the same NAT does,
     * on one connection; a server's clear HTTP on ::1 is another address.
     */
    neighbour(url: string, { requests = 1, everyMs = 2500 } = {}) {
      neighbours.push(
        Bun.spawn([process.execPath, "-e", NEIGHBOUR], {
          env: {
            ...process.env,
            NEIGHBOUR_URL: url,
            NEIGHBOUR_REQUESTS: String(requests),
            NEIGHBOUR_EVERY: String(everyMs),
          },
          stdout: "ignore",
          stderr: "ignore",
        }),
      );
    },
    async stopNeighbours() {
      const stopping = neighbours.splice(0);
      for (const child of stopping) child.kill();
      await Promise.all(stopping.map((child) => child.exited));
    },
    close() {
      return (closing ??= (async () => {
        await fleet.stopNeighbours();
        for (const child of fleet.children) child.kill();
        await Promise.all(fleet.children.map((child) => child.exited));
        if (current !== owner) return;
        current = undefined;
        tc("qdisc del dev lo root");
        tc("qdisc del dev lo ingress");
      })());
    },
  };
  owner.close = fleet.close;
  const failed = launches.find((launch) => launch.status === "rejected");
  if (failed) {
    await fleet.close();
    throw failed.reason;
  }
  return fleet;
}

const NEIGHBOUR = `
const { NEIGHBOUR_URL: url, NEIGHBOUR_REQUESTS: requests, NEIGHBOUR_EVERY: every } = process.env;
const options = { tls: { rejectUnauthorized: false }, ...(url.startsWith("https:") && { protocol: "http3" }) };
for (let i = 0; i < +requests; i++)
  void (async () => {
    for (;;) {
      try {
        const reader = (await fetch(url, options)).body.getReader();
        while (!(await reader.read()).done) await Bun.sleep(50);
      } catch {}
      await Bun.sleep(+every);
    }
  })();`;

function countWorkers() {
  let created = 0;
  window.Worker = class extends Worker {
    constructor(...args: ConstructorParameters<typeof Worker>) {
      super(...args);
      created++;
    }
  };
  Object.defineProperty(window, "__gmWorkers", { get: () => created });
}

/**
 * Opens the app at `origin`, the fleet's first server's by default, with every server selected, signed in when the fleet
 * has a password, and waits for its checks.
 */
export async function openApp(
  page: Page,
  fleet: Fleet,
  config: Partial<TestConfig> = {},
  origin = fleet.servers[0]!.url,
) {
  await page.clearStorage(fleet.servers.flatMap((server) => [server.http, server.url]));
  await page.addInitScript(countWorkers);
  await open(page, fleet.signIn ? `${origin}/login` : origin, {
    servers: fleet.servers.map(({ id, url }) => ({ id, url: id === "self" ? origin : url })),
    config: { ...baseConfig, ...config },
  });
  if (fleet.signIn) {
    await page.getByRole("textbox", { name: "Operator password" }).fill(password);
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
  }
  // Firefox's WebTransport checks behind a shaped link can take longer than the suite's usual 15 s.
  await ready(page, 30_000);
}

/** A run's cost on one server, from its connection log. */
export interface ServerBudget {
  id: string;
  /** Latency buses opened: WebSocket or WebTransport pings. */
  buses: number;
  /** The most QUIC connections opened in the run the server held at once, and what the run's paths need. */
  quic: number;
  quicPlan: number;
  /** Download lane requests, one per lane and stage; upload POSTs, several per lane, and their CORS preflights. */
  downloads: number;
  uploads: number;
  uploadPreflights: number;
  /** Lane requests that reached the TCP side of the HTTP/3 port, which serves no transfer. */
  companionLanes: number;
  /** Answers 429 or 503. */
  refused: number;
}
export interface Attempt {
  /** The result this run saved, or none when it ended without one. */
  record?: HistoryRecord;
  /** What the dial's footer said as the run ended. */
  notice: string;
  /** Workers the page created during the run. */
  workers: number;
  servers: ServerBudget[];
}

const QUIC = /quic:\s+connection from (\S+) (accepted|ended): \d+ held/;
const REQUEST = /request:\s+(\S+) (\S+) from \S+ on (\S+): (\d+)/;

function budget(log: ConnectionLog, from: number, to: number) {
  // The run's own connections: those accepted inside its window, by address and port.
  const own = new Set<string>();
  const counts = {
    buses: 0,
    quic: 0,
    downloads: 0,
    uploads: 0,
    uploadPreflights: 0,
    companionLanes: 0,
    refused: 0,
  };
  for (const line of log.lines(from, to)) {
    const [, peer, event] = QUIC.exec(line) ?? [];
    if (event === "accepted") own.add(peer!);
    if (event === "ended") own.delete(peer!);
    counts.quic = Math.max(counts.quic, own.size);
    const [, method, path, endpoint, status] = REQUEST.exec(line) ?? [];
    if (/^\/(ws|wt)\/ping$/.test(path ?? "") && /^(101|200)$/.test(status!)) counts.buses++;
    if (path === "/download" && method === "GET") counts.downloads++;
    if (path === "/upload") counts[method === "OPTIONS" ? "uploadPreflights" : "uploads"]++;
    if (/^\/(download|upload)$/.test(path ?? "") && endpoint === "H3Companion")
      counts.companionLanes++;
    if (status === "429" || status === "503") counts.refused++;
  }
  return counts;
}

/** Starts a run; `end` waits until it ends, saved or not, with what it cost each server. */
export async function start(page: Page, fleet: Fleet) {
  const marks = fleet.logs.map((log) => log.mark());
  const workers = await page.evaluate<number>("window.__gmWorkers");
  const startedAt = Date.now();
  await runButton(page, /^(Start test|Run again)$/).click();
  const running = () => page.evaluate(() => !!document.querySelector(".run-button.running"));
  await expect.poll(running, { timeout: 10_000 }).toBe(true);
  return {
    async end(timeout = 90_000): Promise<Attempt> {
      await expect.poll(running, { timeout }).toBe(false);
      let record: HistoryRecord | undefined;
      // A result is saved shortly after the run ends; a start that failed saves none.
      for (let wait = 0; wait < 20 && (record?.completedAt ?? -1) < startedAt; wait++) {
        if (wait) await Bun.sleep(100);
        record = await page.evaluate(readLatest, HISTORY_DB);
      }
      if ((record?.completedAt ?? -1) < startedAt) record = undefined;
      // The servers' log writers lag their work a little.
      await Bun.sleep(300);
      const summaries = record?.result.multiServer.servers ?? [];
      return {
        record,
        notice: await page.evaluate(
          () => document.querySelector(".gauge-footer")?.textContent?.trim() ?? "",
        ),
        workers: (await page.evaluate<number>("window.__gmWorkers")) - workers,
        servers: fleet.servers.map((server, i) => {
          const counts = budget(fleet.logs[i]!, marks[i]!, fleet.logs[i]!.mark());
          const summary = summaries.find((entry) => entry.server.id === server.id);
          // Firefox opens a speculative twin, which nothing uses, for every WebTransport dial.
          const dial = browser === "firefox" ? 2 : 1;
          // A WebTransport session rides QUIC whatever HTTP version the browser reported for its origin.
          const { protocol, transport } = summary?.throughput ?? {};
          const transfer =
            protocol === "http3" || (transport && transport !== "fetch-stream") ? 2 : 0;
          const sessions = summary?.throughput.transport === "webtransport" ? dial : 1;
          const bus = summary?.latencyTarget?.transport === "webtransport" ? dial : 0;
          // A run that saved nothing for the server names no paths to plan for.
          const quicPlan = summary ? transfer * sessions + bus : Infinity;
          return { id: server.id, ...counts, quicPlan };
        }),
      };
    },
  };
}

export const attempt = async (page: Page, fleet: Fleet) => (await start(page, fleet)).end();

/** Waits until the running test shows `stage`. */
export const reached = (page: Page, stage: string) =>
  expect(page.locator(`#console[data-phase="${stage}"]`)).toHaveCount(1, { timeout: 20_000 });

/** Every stage of every server measured: the run saved a complete result. */
export function measured(run: Attempt) {
  const failures = run.record?.result.multiServer.failures ?? [];
  return {
    outcome: run.record?.result.outcome ?? `no result: ${run.notice}`,
    failures: failures.map(
      ({ serverId, stage, reason, message }) => `${serverId} ${stage} ${reason}: ${message}`,
    ),
  };
}

/**
 * The connection budget runs must keep (T7), for the runs of each page: after a page's first run no worker is created;
 * per server at most one latency bus, QUIC connections within the paths' plan, and no preflight per upload POST: a
 * lane posts several times to one URL, whose preflight the browser caches.
 */
function overBudget(pages: Attempt[][]) {
  return pages.flatMap((runs, p) =>
    runs.flatMap((run, i) => {
      const at = `page ${p + 1} run ${i + 1}`;
      return [
        ...(i > 0 && run.workers ? [`${at} created ${run.workers} workers`] : []),
        ...run.servers.flatMap((server) =>
          [
            server.buses > 1 && `${server.buses} latency buses`,
            server.quic > server.quicPlan &&
              `${server.quic} QUIC connections held, ${server.quicPlan} planned`,
            2 * server.uploadPreflights > server.uploads &&
              `${server.uploadPreflights} preflights for ${server.uploads} upload POSTs`,
          ]
            .filter(Boolean)
            .map((problem) => `${at} ${server.id}: ${problem}`),
        ),
      ];
    }),
  );
}

/** T7 over the runs a file's scenarios recorded, one list per page; workers are judged from a page's second run. */
export function budgetTest(runs: string, pages: Attempt[][]) {
  test(`T7: ${runs} keep one bus, their workers and the QUIC plan`, () => {
    expect(pages.flat().length).toBeGreaterThan(0);
    expect(overBudget(pages)).toEqual([]);
  });
}
