import {
  baseConfig,
  frankfurt,
  open,
  phase,
  ready,
  runButton,
  savedResult,
  spawnPeer,
} from "./fleet";
import { incoherence } from "../src/lib/history/types";
import type { Server } from "./servers";
import { expect, test, type Page } from "./webview";

type Peer = Awaited<ReturnType<typeof spawnPeer>>;
interface Fault {
  name: string;
  downloadMs?: number;
  uploadMs?: number;
  /** Acts once the named stage has run for 300 ms. */
  during: "download" | "upload";
  act?(page: Page, victim: Peer): Promise<void>;
  /** A Settings edit of the active download's duration, a second later. */
  editDownloadMs?: number;
  outcome: "partial" | "incomplete" | "complete";
  failure: { serverId: string; stage: string; reason: string } | null;
}

const faults: Fault[] = [
  {
    name: "a stopped peer leaves the download it stalled",
    downloadMs: 3_000,
    during: "download",
    act: async (_page, victim) => void victim.kill("SIGSTOP"),
    outcome: "partial",
    failure: { serverId: "oslo", stage: "download", reason: "timeout" },
  },
  {
    name: "a killed peer leaves the upload it was in",
    uploadMs: 3_000,
    during: "upload",
    act: async (_page, victim) => void victim.kill("SIGKILL"),
    outcome: "partial",
    failure: { serverId: "oslo", stage: "upload", reason: "connection-lost" },
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

const entry = ({ id, name, url }: Server) => ({ id, name, url });

async function editDownload(page: Page, value: number) {
  await page.evaluate((value) => {
    const input = [
      ...document.querySelectorAll<HTMLInputElement>(".duration-fields input"),
    ].find((field) =>
      field.closest("label")?.textContent?.startsWith("Download"),
    )!;
    input.value = String(value);
    input.dispatchEvent(new Event("change", { bubbles: true }));
  }, value);
}

for (const fault of faults)
  test(fault.name, async (page) => {
    const victim = await spawnPeer("Oslo");
    const peers = [frankfurt, victim.server];
    const self = await spawnPeer("Bergen", {
      GM_SERVER_CATALOG: JSON.stringify({ servers: peers.map(entry) }),
    });
    const config = {
      ...baseConfig,
      duration: {
        ...baseConfig.duration,
        downloadMs: fault.downloadMs ?? 1_000,
        uploadMs: fault.uploadMs ?? 1_000,
      },
    };
    try {
      await open(page, self.server.url, {
        servers: [{ id: "self", url: self.server.url }, ...peers],
        config,
      });
      await ready(page);
      const startedAt = Date.now();
      await runButton(page, "Start test").click();
      if (fault.editDownloadMs)
        await page.getByRole("button", { name: "Open settings" }).click();
      await expect(phase(page, fault.during)).toHaveCount(1, {
        timeout: 15_000,
      });
      await Bun.sleep(300);
      await fault.act?.(page, victim);
      if (fault.editDownloadMs) {
        await Bun.sleep(1_000);
        await editDownload(page, fault.editDownloadMs);
        config.duration.downloadMs = fault.editDownloadMs;
      }
      const saved = await savedResult(page, startedAt, 30_000);
      expect(incoherence(saved, { ...config, adaptive: false })).toEqual([]);
      expect(saved.outcome).toBe(fault.outcome);
      const failures = saved
        .multiServer!.failures.filter(
          (failure) => failure.scope === "throughput",
        )
        .map(({ serverId, stage, reason }) => ({ serverId, stage, reason }));
      if (fault.failure) expect(failures).toContainEqual(fault.failure);
      else expect(failures).toEqual([]);
    } finally {
      victim.kill("SIGCONT");
      victim.kill();
      self.kill();
    }
  });
