import {
  baseConfig,
  catalog,
  frankfurt,
  open,
  phase,
  ready,
  run,
  runButton,
  savedResult,
  spawnPeer,
  plansOf,
} from "./fleet";
import { incoherence } from "../src/engine/run/result";
import { launch } from "./servers";
import { browser, expect, test, type Page } from "./webview";

type Peer = Awaited<ReturnType<typeof spawnPeer>>;
interface Fault {
  name: string;
  downloadMs?: number;
  uploadMs?: number;
  /** Acts once the named stage has measured for 300 ms: its own time left shows, not the last stage's handing off. */
  during: "download" | "upload";
  act?(page: Page, victim: Peer): Promise<{ kill(): void } | void>;
  /** A Settings edit of the active download's duration, a second after the fault's moment. */
  editDownloadMs?: number;
  outcome: "partial" | "incomplete" | "complete";
  failure: { serverId: string; stage: string; reason: string } | null;
  interval?: { stage: string; reason: string };
}

const faults: Fault[] = [
  {
    name: "a stopped peer leaves the download it stalled",
    // The fault lands once the page shows the stage, which a busy browser can show late; a stall must still end the
    // window well before it closes to fail the stage rather than the next one.
    downloadMs: 5_000,
    during: "download",
    act: async (_page, victim) => void victim.kill("SIGSTOP"),
    outcome: "partial",
    failure: { serverId: "oslo", stage: "download", reason: "timeout" },
  },
  {
    name: "a restarted peer resumes the upload under a replacement receiver",
    uploadMs: 4_000,
    during: "upload",
    async act(_page, victim) {
      victim.kill("SIGKILL");
      await victim.exited;
      const started = Date.now();
      const relaunched = await launch(JSON.parse(process.env.GM_E2E_LAUNCH!), victim.server);
      if (Date.now() - started > 1_000) throw new Error("the peer took over a second to relaunch");
      return relaunched;
    },
    outcome: "complete",
    failure: null,
    interval: { stage: "upload", reason: "evidence-resumed" },
  },
  {
    name: "a frozen page never folds the gap into its download",
    downloadMs: 1_500,
    during: "download",
    async act(page) {
      await page.cdp("Page.setWebLifecycleState", { state: "frozen" });
      await Bun.sleep(2_000);
      await page.cdp("Page.setWebLifecycleState", { state: "active" });
    },
    outcome: "incomplete",
    failure: {
      serverId: "self",
      stage: "download",
      reason: "insufficient-evidence",
    },
  },
  {
    name: "shortening the active download ends it with its evidence",
    downloadMs: 3_000,
    during: "download",
    editDownloadMs: 1_000,
    outcome: "complete",
    failure: null,
  },
];

test.chrome("a download graph leaves separate curves and fills across a page-clock gap", async (page) => {
  await open(page, undefined, {
    config: {
      stages: { latency: false, download: true, upload: false, bidirectional: false },
      duration: { ...baseConfig.duration, downloadMs: 5_000 },
    },
  });
  await ready(page);
  const startedAt = Date.now();
  await runButton(page, "Start test").click();
  await expect
    .poll(() =>
      page.evaluate(() => document.querySelector<SVGPathElement>(".graph .plot path.line")?.getTotalLength() ?? 0),
    )
    .toBeGreaterThan(0);
  // Pause the page while its transfer worker keeps running, then let the visible graph resume drawing.
  await page.evaluate(() => {
    const until = performance.now() + 2_000;
    while (performance.now() < until) {}
  });
  await savedResult(page, startedAt, 20_000);
  await expect
    .poll(() =>
      page.evaluate(
        () => (document.querySelector(".graph .plot path.line")?.getAttribute("d")?.match(/M/g) ?? []).length >= 2,
      ),
    )
    .toBe(true);
  const [line, area] = await page.evaluate(() =>
    [".graph .plot path.line", ".graph .plot path.area"].map(
      (selector) => document.querySelector(selector)?.getAttribute("d") ?? "",
    ),
  );
  expect((line!.match(/M/g) ?? []).length).toBeGreaterThanOrEqual(2);
  expect((area!.match(/M/g) ?? []).length).toBe((line!.match(/M/g) ?? []).length);
  expect((area!.match(/Z/g) ?? []).length).toBe((line!.match(/M/g) ?? []).length);
});

/** Waits until the running stage, planned for `stageMs`, has measured for `ms`, as its time left shows. */
async function measuredFor(page: Page, stageMs: number, ms: number) {
  const left = () =>
    page.evaluate(() => parseFloat(document.querySelector(".remaining:not(.handoff-out) .figure")?.textContent ?? ""));
  // A stage just begun can still show the time its predecessor ended on.
  await expect
    .poll(
      async () => {
        const seconds = await left();
        return seconds > 0 && seconds <= (stageMs - ms) / 1000;
      },
      { timeout: 15_000 },
    )
    .toBe(true);
}

async function editDownload(page: Page, value: number) {
  await page.evaluate((value) => {
    const input = document.querySelector<HTMLInputElement>('input[aria-label="Download stage time"]')!;
    input.value = String(value / 1000);
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }, value);
}

// Freezing a page is Chrome's lifecycle control; no other browser offers it.
for (const fault of faults.filter((fault) => browser === "chrome" || !fault.name.includes("frozen")))
  test(fault.name, async (page) => {
    const victim = await spawnPeer("Oslo");
    const peers = [frankfurt, victim.server];
    const self = await spawnPeer("Bergen", catalog(...peers));
    const config = {
      ...baseConfig,
      duration: {
        ...baseConfig.duration,
        downloadMs: fault.downloadMs ?? 1_000,
        uploadMs: fault.uploadMs ?? 1_000,
      },
    };
    const relaunched: { kill(): void }[] = [];
    try {
      await open(page, self.server.url, {
        servers: [{ id: "self", url: self.server.url }, ...peers],
        config,
      });
      await ready(page);
      const startedAt = Date.now();
      await runButton(page, "Start test").click();
      if (fault.editDownloadMs) await page.getByRole("button", { name: "Settings", exact: true }).click();
      await expect(phase(page, fault.during)).toHaveCount(1, {
        timeout: 15_000,
      });
      const stageMs = config.duration[`${fault.during}Ms`];
      await measuredFor(page, stageMs, 300);
      const replacement = await fault.act?.(page, victim);
      if (replacement) relaunched.push(replacement);
      if (fault.editDownloadMs) {
        await measuredFor(page, stageMs, 1_300);
        await editDownload(page, fault.editDownloadMs);
        config.duration.downloadMs = fault.editDownloadMs;
      }
      const saved = await savedResult(page, startedAt, 30_000);
      expect(incoherence(saved.result, plansOf({ ...config, adaptive: false }))).toEqual([]);
      const { failures } = saved.result.multiServer;
      if (fault.failure)
        expect(
          failures
            .filter((failure) => failure.scope === "throughput")
            .map(({ serverId, stage, reason }) => ({ serverId, stage, reason })),
        ).toContainEqual(fault.failure);
      else expect(failures).toEqual([]);
      expect(saved.result.outcome).toBe(fault.outcome);
      // Short of complete, the dial says so and names the stage it lacks, for as long as the result shows.
      if (fault.failure) {
        const stage = fault.failure.stage[0]!.toUpperCase() + fault.failure.stage.slice(1);
        await expect(page.locator(".gauge-status.warn")).toHaveText(
          fault.outcome === "partial" ? "Partial" : "Incomplete",
        );
        await expect(page.locator(".gauge-hint")).toContainText(stage);
      }
      if (fault.interval)
        expect(
          saved.result.multiServer.intervals.map(({ stage, reason }) => ({
            stage,
            reason,
          })),
        ).toContainEqual(fault.interval);
    } finally {
      victim.kill("SIGCONT");
      victim.kill();
      for (const child of relaunched) child.kill();
      self.kill();
    }
  });

test("a peer at capacity while its upload prepares leaves as busy", async (page) => {
  const busy = await spawnPeer("Oslo", {
    GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT: "2",
    GM_MAX_SESSIONS_PER_CLIENT: "2",
  });
  const self = await spawnPeer("Bergen", catalog(busy.server));
  const held = new AbortController();
  try {
    await open(page, self.server.url, {
      servers: [{ id: "self", url: self.server.url }, busy.server],
      config: {
        ...baseConfig,
        stages: { ...baseConfig.stages, latency: false, download: false },
      },
    });
    await ready(page);
    for (let i = 0; i < 2; i++) {
      const hold = await fetch(`${busy.server.http}/download?bytes=${2 ** 40}`, {
        signal: held.signal,
      });
      expect(hold.status).toBe(200);
      const reader = hold.body!.getReader();
      void (async () => {
        while (!(await reader.read()).done) await Bun.sleep(100);
      })().catch(() => {});
    }
    const saved = await run(page, 30_000);
    expect(saved.result.outcome).toBe("partial");
    expect(
      saved.result.multiServer.failures.map(({ serverId, stage, reason }) => ({
        serverId,
        stage,
        reason,
      })),
    ).toContainEqual({
      serverId: "oslo",
      stage: "upload",
      reason: "server-busy",
    });
  } finally {
    held.abort();
    busy.kill();
    self.kill();
  }
});
