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
import type { Subprocess } from "bun";
import type { Server } from "./servers";
import { expect, test, type Page } from "./webview";

const long = {
  ...baseConfig,
  duration: { ...baseConfig.duration, downloadMs: 1500, uploadMs: 1000 },
};

const entry = ({ id, name, url }: Server) => ({ id, name, url });
const catalog = (...servers: Server[]) => ({
  GM_SERVER_CATALOG: JSON.stringify({ servers: servers.map(entry) }),
});

async function killDuringDownload(page: Page, ...peers: Subprocess[]) {
  await runButton(page, "Start test").click();
  await expect(phase(page, "download")).toHaveCount(1, { timeout: 10_000 });
  await Bun.sleep(500);
  for (const peer of peers) peer.kill("SIGKILL");
}

test("a peer dropout keeps healthy transfers and persists its failure", async (page) => {
  const oslo = await spawnPeer("Oslo");
  const servers = [frankfurt, oslo.server];
  const bergen = await spawnPeer("Bergen", catalog(...servers));
  try {
    await open(page, bergen.server.url, {
      servers: [{ id: "self", url: bergen.server.url }, ...servers],
      config: long,
    });
    await ready(page);
    const startedAt = Date.now();
    await killDuringDownload(page, oslo);
    const saved = await savedResult(page, startedAt, 30_000);
    expect(saved.outcome).toBe("partial");
    expect(saved.multiServer?.participants).toEqual(["self", "server-1"]);
    const failure = saved.multiServer?.failures.find(
      (candidate) => candidate.scope === "throughput",
    );
    expect(failure?.serverId).toBe("oslo");
    const later = saved.multiServer!.intervals.filter(
      (interval) =>
        interval.reason === "dropout" || interval.stage !== "download",
    );
    expect(later.length).toBeGreaterThan(0);
    for (const interval of later)
      expect(interval.participants).not.toContain("oslo");
    expect(saved.stages.upload.result?.reportedBytesPerSec).toBeGreaterThan(0);

    await page.evaluate((id) => (location.hash = `/history/${id}`), saved.id);
    await page.reload();
    await expect(page.locator(".result-detail .summary-scope")).toContainText(
      "2 of 3 servers",
    );
    expect((await savedResult(page)).multiServer).toEqual(saved.multiServer);
  } finally {
    oslo.kill();
    bergen.kill();
  }
});

test("losing every server ends Incomplete without an Aborted flash", async (page) => {
  const porto = await spawnPeer("Porto");
  const lisbon = await spawnPeer("Lisbon", catalog(porto.server));
  try {
    await open(page, lisbon.server.url, {
      servers: [{ id: "self", url: lisbon.server.url }, porto.server],
      config: long,
    });
    await ready(page);
    await page.evaluate(() => {
      const main = document.querySelector<HTMLElement>("#console")!;
      const phases: string[] = ((window as any).phases = []);
      new MutationObserver(() => phases.push(main.dataset.phase!)).observe(
        main,
        { attributeFilter: ["data-phase"] },
      );
    });
    await killDuringDownload(page, porto, lisbon);
    await expect(page.locator("footer.status .label")).toHaveText(
      "Incomplete",
      { timeout: 20_000 },
    );
    expect(await page.evaluate(() => (window as any).phases)).not.toContain(
      "aborted",
    );
  } finally {
    porto.kill();
    lisbon.kill();
  }
});
