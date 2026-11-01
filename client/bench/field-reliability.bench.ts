// A diagnostic sweep: several servers share one constrained, lossy access link.
import { appendFileSync } from "node:fs";
import {
  attempt,
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

const output = process.env.GM_DIAG_OUTPUT ?? "/tmp/graphite-reliability.jsonl";
const count = Number(process.env.GM_DIAG_SERVERS ?? 1);
const target = process.env.GM_DIAG_TARGET ?? "protocol:http1";
const repeats = Number(process.env.GM_DIAG_REPEATS ?? 3);
const loss = Number(process.env.GM_DIAG_LOSS ?? 0.3);
const shared = process.env.GM_DIAG_SHARED !== "0";
const vary = process.env.GM_DIAG_VARY === "1";
const native = process.env.GM_DIAG_CLIENT === "native";
const stageMs = Number(process.env.GM_DIAG_STAGE_MS ?? (vary ? 5000 : 3000));
const streams = Number(process.env.GM_DIAG_STREAMS ?? 0);
if (!Number.isInteger(count) || count < 1 || count > 4)
  throw new Error("GM_DIAG_SERVERS must be 1 to 4");
if (!Number.isInteger(repeats) || repeats < 1 || repeats > 100)
  throw new Error("GM_DIAG_REPEATS must be 1 to 100");
if (!Number.isFinite(loss) || loss < 0 || loss > 100)
  throw new Error("GM_DIAG_LOSS must be a percentage from 0 to 100");
if (!Number.isInteger(stageMs) || stageMs < 3000 || stageMs > 60_000)
  throw new Error("GM_DIAG_STAGE_MS must be 3000 to 60000");
if (!Number.isInteger(streams) || streams < 0 || streams > 128)
  throw new Error("GM_DIAG_STREAMS must be 0 to 128");
const direction = (delayMs: number, mbit: number) => ({ delayMs, mbit, lossPercent: loss });
const links: Link[] = Array.from({ length: count }, () => ({
  down: direction(50, 30),
  up: direction(50, 8),
}));
const log = (data: unknown) => appendFileSync(output, `${JSON.stringify(data)}\n`);

test(
  "diagnose reliability on a constrained access link",
  async (page) => {
    const fleet = await shapedFleet(links);
    try {
      if (shared) {
        const tc = (...args: string[]) => {
          const result = Bun.spawnSync(["tc", ...args], { stderr: "pipe" });
          if (result.exitCode) throw new Error(`tc ${args.join(" ")}: ${result.stderr}`);
        };
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
        await openApp(
          page,
          fleet,
          runConfig(target, {
            transports: { throughputTarget: target, latencyTarget: "transport:websocket" },
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
        );
      }
      for (let repeat = 1; repeat <= repeats; repeat++) {
        const started = Date.now();
        const queues: { atMs: number; label: string; qdisc: string }[] = [];
        const sampleQueue = (label: string) => {
          const stats = Bun.spawnSync(["tc", "-s", "qdisc", "show", "dev", "lo"]);
          queues.push({ atMs: Date.now() - started, label, qdisc: stats.stdout.toString() });
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
                "websocket",
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
            if (vary) {
              const deadline = Date.now() + 60_000;
              while (
                child.exitCode === null &&
                Date.now() < deadline &&
                !fleet.logs.some((entry, i) =>
                  entry.lines(marks[i]).some((line) => line.includes("GET /download")),
                )
              )
                await Bun.sleep(100);
              if (child.exitCode === null && Date.now() < deadline) {
                await Bun.sleep(1000);
                sampleQueue("before slowdown");
                fleet.reshape(0, {
                  down: { delayMs: 50, mbit: 5, lossPercent: 1.5 },
                  up: { delayMs: 50, mbit: 1, lossPercent: 1.5 },
                });
                await Bun.sleep(2000);
                sampleQueue("before recovery");
                fleet.reshape(0, links[0]!);
              }
            }
            const timer = setTimeout(() => child.kill(), 120_000);
            const exitCode = await child.exited;
            clearTimeout(timer);
            log({
              count,
              target,
              loss,
              vary,
              shared,
              repeat,
              client: "native",
              stageMs,
              streams,
              startedAt: new Date(started).toISOString(),
              elapsedMs: Date.now() - started,
              exitCode,
              stdout: await stdout,
              stderr: await stderr,
              queues,
            });
            continue;
          }
          let result: Attempt;
          if (vary) {
            const running = await start(page, fleet);
            await reached(page, "download");
            await Bun.sleep(1000);
            sampleQueue("before slowdown");
            fleet.reshape(0, {
              down: { delayMs: 50, mbit: 5, lossPercent: 1.5 },
              up: { delayMs: 50, mbit: 1, lossPercent: 1.5 },
            });
            await Bun.sleep(2000);
            sampleQueue("before recovery");
            fleet.reshape(0, links[0]!);
            result = await running.end();
          } else result = await attempt(page, fleet);
          log({
            count,
            target,
            loss,
            vary,
            shared,
            stageMs,
            streams,
            startedAt: new Date(started).toISOString(),
            repeat,
            elapsedMs: Date.now() - started,
            ...measured(result),
            servers: result.servers,
            paths: result.record?.result.multiServer.servers.map(
              ({ server, throughput, stages }) => ({
                id: server.id,
                throughput,
                stages,
              }),
            ),
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
            loss,
            shared,
            repeat,
            elapsedMs: Date.now() - started,
            error: String(error),
            page: await page.evaluate(() => document.body.textContent),
          });
        }
      }
      const statistics = Bun.spawnSync(["tc", "-s", "qdisc", "show", "dev", "lo"]);
      log({ count, target, loss, shared, vary, qdisc: statistics.stdout.toString() });
    } catch (error) {
      log({
        count,
        target,
        loss,
        shared,
        phase: "preparation",
        error: String(error),
        page: await page.evaluate(() => document.body.textContent).catch(() => "unavailable"),
      });
      throw error;
    } finally {
      await fleet.close();
    }
  },
  { timeout: Math.max(300_000, repeats * (stageMs * 2 + 30_000)) },
);
