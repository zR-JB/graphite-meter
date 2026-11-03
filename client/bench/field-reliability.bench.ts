// A diagnostic sweep: several servers share one constrained, lossy access link.
import { closeSync, constants, fchmodSync, openSync, writeFileSync } from "node:fs";
import {
  attempt,
  budget,
  measured,
  openApp,
  reached,
  runConfig,
  shapedFleet,
  start,
  type Attempt,
  type Link,
} from "../e2e/network/harness";
import { test } from "../e2e/webview";

const output = "/tmp/graphite-reliability.jsonl";
const label = process.env.GM_DIAG_LABEL ?? "";
const count = Number(process.env.GM_DIAG_SERVERS ?? 1);
const target = process.env.GM_DIAG_TARGET ?? "protocol:http1";
const latencyTarget = process.env.GM_DIAG_LATENCY ?? "transport:webtransport";
const repeats = Number(process.env.GM_DIAG_REPEATS ?? 3);
const loss = Number(process.env.GM_DIAG_LOSS ?? 0.3);
const shared = process.env.GM_DIAG_SHARED !== "0";
const vary = process.env.GM_DIAG_VARY === "1";
const native = process.env.GM_DIAG_CLIENT === "native";
const stageMs = Number(process.env.GM_DIAG_STAGE_MS ?? (vary ? 5000 : 3000));
const streams = Number(process.env.GM_DIAG_STREAMS ?? 0);
const outageMs = Number(process.env.GM_DIAG_UDP_OUTAGE_MS ?? 0);
const outageServer = Number(process.env.GM_DIAG_UDP_OUTAGE_SERVER ?? count);
const outagePhase = process.env.GM_DIAG_UDP_OUTAGE_PHASE ?? "download";
const downMbps = Number(process.env.GM_DIAG_DOWN_MBPS ?? 30);
const upMbps = Number(process.env.GM_DIAG_UP_MBPS ?? 8);
const rttMs = Number(process.env.GM_DIAG_RTT_MS ?? 100);
const sampleQueues = process.env.GM_DIAG_QUEUE === "1";
const sampleIntervals = process.env.GM_DIAG_INTERVALS === "1";
const queueLimit = Number(process.env.GM_DIAG_UP_QUEUE_PACKETS ?? 1000);
if (!Number.isInteger(count) || count < 1 || count > 4)
  throw new Error("GM_DIAG_SERVERS must be 1 to 4");
if (!["transport:websocket", "transport:webtransport"].includes(latencyTarget))
  throw new Error("GM_DIAG_LATENCY must select websocket or webtransport");
if (!Number.isInteger(repeats) || repeats < 1 || repeats > 100)
  throw new Error("GM_DIAG_REPEATS must be 1 to 100");
if (!Number.isFinite(loss) || loss < 0 || loss > 100)
  throw new Error("GM_DIAG_LOSS must be a percentage from 0 to 100");
if (!Number.isInteger(stageMs) || stageMs < 3000 || stageMs > 60_000)
  throw new Error("GM_DIAG_STAGE_MS must be 3000 to 60000");
if (!Number.isInteger(streams) || streams < 0 || streams > 128)
  throw new Error("GM_DIAG_STREAMS must be 0 to 128");
if (!Number.isInteger(outageMs) || outageMs < 0 || outageMs > 10_000)
  throw new Error("GM_DIAG_UDP_OUTAGE_MS must be 0 to 10000");
if (!Number.isInteger(outageServer) || outageServer < 0 || outageServer > count)
  throw new Error("GM_DIAG_UDP_OUTAGE_SERVER must be 0 or identify a selected server");
if (!["startup", "download"].includes(outagePhase))
  throw new Error("GM_DIAG_UDP_OUTAGE_PHASE must be startup or download");
if (native && outagePhase === "startup")
  throw new Error("startup UDP outages currently require the browser client");
if (![downMbps, upMbps].every((rate) => Number.isFinite(rate) && rate > 0 && rate <= 10_000))
  throw new Error("GM_DIAG_DOWN_MBPS and GM_DIAG_UP_MBPS must be positive rates up to 10000");
if (!Number.isFinite(rttMs) || rttMs < 0 || rttMs > 10_000)
  throw new Error("GM_DIAG_RTT_MS must be from 0 to 10000");
if (
  !Number.isInteger(queueLimit) ||
  queueLimit < 1 ||
  queueLimit > 1000 ||
  (!shared && queueLimit !== 1000) ||
  (vary && queueLimit !== 1000)
)
  throw new Error("GM_DIAG_UP_QUEUE_PACKETS must be 1 to 1000 on a steady shared link");
const direction = (delayMs: number, mbit: number) => ({ delayMs, mbit, lossPercent: loss });
const links: Link[] = Array.from({ length: count }, () => ({
  down: direction(rttMs / 2, downMbps),
  up: direction(rttMs / 2, upMbps),
}));
const log = (data: object) => {
  const file = openSync(
    output,
    constants.O_WRONLY | constants.O_CREAT | constants.O_APPEND | constants.O_NOFOLLOW,
    0o600,
  );
  try {
    fchmodSync(file, 0o600);
    writeFileSync(
      file,
      `${JSON.stringify({ label, outagePhase, queueLimit, sampleQueues, ...data })}\n`,
    );
  } finally {
    closeSync(file);
  }
};

test(
  "diagnose reliability on a constrained access link",
  async (page) => {
    const fleet = await shapedFleet(links);
    let startupOutage: Promise<void> | undefined;
    try {
      const outage = async () => {
        await fleet.udpOutage(outageServer ? fleet.servers[outageServer - 1]! : null, outageMs);
      };
      if (shared) {
        const tc = (...args: string[]) => {
          const result = Bun.spawnSync(["tc", ...args], { stderr: "pipe" });
          if (result.exitCode) throw new Error(`tc ${args.join(" ")}: ${result.stderr}`);
        };
        if (queueLimit !== 1000)
          tc(
            "qdisc",
            "change",
            "dev",
            "lo",
            "parent",
            "1:5",
            "handle",
            "50:",
            "netem",
            "delay",
            `${rttMs / 2}ms`,
            "loss",
            `${loss}%`,
            "rate",
            `${upMbps}mbit`,
            "limit",
            String(queueLimit),
          );
        tc("filter", "del", "dev", "lo", "parent", "1:", "protocol", "ip", "prio", "1");
        for (const server of fleet.servers) {
          const address = new URL(server.url).hostname;
          for (const [side, band] of [
            ["src", "4"],
            ["dst", "5"],
          ] as const)
            tc(
              "filter",
              "add",
              "dev",
              "lo",
              "parent",
              "1:",
              "protocol",
              "ip",
              "prio",
              "1",
              "u32",
              "match",
              "ip",
              side,
              `${address}/32`,
              "flowid",
              `1:${band}`,
            );
        }
      }
      if (!native) {
        await page.addInitScript(() => {
          const commands: unknown[] = [];
          Object.assign(window, { __gmDownloadCommands: commands });
          const post = Worker.prototype.postMessage;
          Worker.prototype.postMessage = function (message, ...options) {
            if (message?.type === "download")
              commands.push({ url: message.url, timed: message.timed, until: message.until });
            return Reflect.apply(post, this, [message, ...options]);
          };
        });
        if (outageMs && outagePhase === "startup") startupOutage = outage();
        await openApp(
          page,
          fleet,
          runConfig(target, {
            transports: { throughputTarget: target, latencyTarget },
            adaptive: false,
            ...(streams > 0 ? { transferStreams: { mode: "forced", count: streams } } : {}),
            duration: {
              warmupMs: 800,
              latencyMs: 2000,
              downloadMs: stageMs,
              uploadMs: stageMs,
              bidirectionalMs: 3000,
            },
          }),
          undefined,
          outagePhase !== "startup",
        );
        await startupOutage;
      }
      for (let repeat = 1; repeat <= repeats; repeat++) {
        const started = Date.now();
        const queues: { atMs: number; label: string; qdisc: string; ingress: string }[] = [];
        const sampleQueue = (label: string) => {
          const stats = Bun.spawnSync(["tc", "-s", "qdisc", "show", "dev", "lo"]);
          const ingress = Bun.spawnSync([
            "tc",
            "-s",
            "filter",
            "show",
            "dev",
            "lo",
            "parent",
            "ffff:",
          ]);
          queues.push({
            atMs: Date.now() - started,
            label,
            qdisc: stats.stdout.toString(),
            ingress: ingress.stdout.toString(),
          });
        };
        const sampler = sampleQueues ? setInterval(() => sampleQueue("periodic"), 1000) : null;
        const impair = async () => {
          await Bun.sleep(1000);
          sampleQueue("before impairment");
          const interrupted = outageMs && outagePhase === "download" ? outage() : null;
          const outageSample =
            outageMs && outagePhase === "download"
              ? (async () => {
                  await Bun.sleep(Math.min(1000, outageMs / 2));
                  sampleQueue("during UDP outage");
                })()
              : null;
          const slowdown = vary
            ? (async () => {
                fleet.reshape(0, {
                  down: { delayMs: rttMs / 2, mbit: Math.min(downMbps, 5), lossPercent: 1.5 },
                  up: { delayMs: rttMs / 2, mbit: Math.min(upMbps, 1), lossPercent: 1.5 },
                });
                await Bun.sleep(2000);
                sampleQueue("before recovery");
                fleet.reshape(0, links[0]!);
              })()
            : null;
          await Promise.all([interrupted, outageSample, slowdown]);
        };
        try {
          if (native) {
            const marks = fleet.logs.map((entry) => entry.mark());
            const protocol = target.replace("protocol:", "");
            const child = Bun.spawn(
              [
                process.env.GM_DIAG_NATIVE_BIN ?? "../rust/target/ci/graphite-meter-client",
                "--url",
                fleet.servers[0]!.url,
                "--insecure",
                "--report",
                "--throughput-transport",
                "fetch-stream",
                "--throughput-protocol",
                protocol,
                "--latency-transport",
                latencyTarget === "transport:webtransport" ? "webtransport" : "websocket",
                "--streams",
                String(streams || (protocol === "http1" ? 4 : 1)),
                "--warmup",
                "800ms",
                "--latency-duration",
                "2s",
                "--download-duration",
                `${stageMs}ms`,
                "--upload-duration",
                `${stageMs}ms`,
                ...fleet.servers.flatMap(({ id }) => ["--server", id]),
              ],
              { stdout: "pipe", stderr: "pipe" },
            );
            const stdout = new Response(child.stdout).text();
            const stderr = new Response(child.stderr).text();
            if (vary || (outageMs && outagePhase === "download")) {
              const deadline = Date.now() + 60_000;
              while (
                child.exitCode === null &&
                Date.now() < deadline &&
                !fleet.logs.some((entry, i) =>
                  entry.lines(marks[i]).some((line) => line.includes("GET /download")),
                )
              )
                await Bun.sleep(100);
              if (child.exitCode === null && Date.now() < deadline) await impair();
            }
            const timer = setTimeout(() => child.kill(), 120_000);
            const exitCode = await child.exited;
            clearTimeout(timer);
            log({
              count,
              target,
              latencyTarget,
              loss,
              vary,
              shared,
              repeat,
              client: "native",
              stageMs,
              streams,
              outageMs,
              outageServer,
              downMbps,
              upMbps,
              rttMs,
              startedAt: new Date(started).toISOString(),
              elapsedMs: Date.now() - started,
              exitCode,
              stdout: await stdout,
              stderr: await stderr,
              servers: fleet.logs.map((entry, i) => ({
                id: fleet.servers[i]!.id,
                ...budget(entry, marks[i]!, entry.mark()),
              })),
              queues,
            });
            continue;
          }
          const handshakes = await page.evaluate<number>("window.__gmFailedHandshakes");
          let result: Attempt;
          if (vary || (outageMs && outagePhase === "download")) {
            const running = await start(page, fleet);
            await reached(page, "download");
            await impair();
            result = await running.end();
          } else result = await attempt(page, fleet);
          log({
            count,
            target,
            latencyTarget,
            loss,
            vary,
            shared,
            stageMs,
            streams,
            outageMs,
            outageServer,
            downMbps,
            upMbps,
            rttMs,
            startedAt: new Date(started).toISOString(),
            repeat,
            elapsedMs: Date.now() - started,
            ...measured(result),
            upload: result.record?.result.upload,
            intervals: sampleIntervals ? result.record?.result.multiServer.intervals : undefined,
            servers: result.servers,
            paths: result.record?.result.multiServer.servers.map(
              ({ server, throughput, latencyTarget: latencyPath, latencyByStage, stages }) => ({
                id: server.id,
                throughput,
                latency: latencyPath,
                latencyByStage,
                stages,
              }),
            ),
            failedHandshakes:
              (await page.evaluate<number>("window.__gmFailedHandshakes")) - handshakes,
            totalFailedHandshakes: await page.evaluate<number>("window.__gmFailedHandshakes"),
            downloads: await page.evaluate(() =>
              (
                window as unknown as { __gmDownloadCommands: unknown[] }
              ).__gmDownloadCommands.splice(0),
            ),
            notice: result.notice,
            queues,
          });
        } catch (error) {
          log({
            count,
            target,
            latencyTarget,
            loss,
            shared,
            repeat,
            elapsedMs: Date.now() - started,
            error: String(error),
            page: await page.evaluate(() => document.body.textContent),
          });
        } finally {
          if (sampler) clearInterval(sampler);
        }
      }
      const statistics = Bun.spawnSync(["tc", "-s", "qdisc", "show", "dev", "lo"]);
      log({
        count,
        target,
        latencyTarget,
        loss,
        shared,
        vary,
        qdisc: statistics.stdout.toString(),
      });
    } catch (error) {
      log({
        count,
        target,
        latencyTarget,
        loss,
        shared,
        phase: "preparation",
        error: String(error),
        page: await page.evaluate(() => document.body.textContent).catch(() => "unavailable"),
      });
      throw error;
    } finally {
      await startupOutage;
      await fleet.close();
    }
  },
  { timeout: Math.max(300_000, repeats * (stageMs * 2 + 30_000)) },
);
