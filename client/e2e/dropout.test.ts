import {
  baseConfig,
  catalog,
  frankfurt,
  open,
  phase,
  ready,
  runButton,
  savedResult,
  spawnPeer,
} from "./fleet";
import type { Subprocess } from "bun";
import { expect, test, type Page } from "./webview";

// The dial shows a rate after half a second of evidence, so a peer killed then leaves survivors the 800 ms a result needs.
const long = {
  ...baseConfig,
  duration: { ...baseConfig.duration, downloadMs: 4000, uploadMs: 1000 },
};

async function killDuringDownload(page: Page, ...peers: Subprocess[]) {
  await runButton(page, "Start test").click();
  await expect(phase(page, "download")).toHaveCount(1, { timeout: 10_000 });
  // Measured evidence exists once the readout shows a rate.
  await expect(page.locator(".gauge-value")).toHaveText(/\d/);
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
    expect(saved.result.outcome).toBe("partial");
    expect(saved.result.multiServer.participants).toEqual(["self", "server-1"]);
    const failure = saved.result.multiServer.failures.find(
      (candidate) => candidate.scope === "throughput",
    );
    expect(failure).toMatchObject({
      serverId: "oslo",
      stage: "download",
      reason: "connection-lost",
    });
    // Upload ran without Oslo: it is partial, says who it lacked, and Oslo alone reads it as not reached.
    await expect(page.locator(".gauge-hint")).toHaveText("Oslo: connection lost in Download");
    expect(saved.result.stages.upload).toBe("partial");
    const card = (stage: string) => page.locator(`.results .card[data-tone="${stage}"]`);
    await expect(card("upload").locator(".name .status")).toHaveText("Partial");
    await expect(card("upload").locator(".line .reason")).toHaveText("Without Oslo");
    await expect(card("download").locator(".line .reason")).toHaveText("Oslo: connection lost");
    await expect(page.getByRole("switch", { name: /^Upload stage/ })).toHaveAttribute(
      "class",
      /\bchip--partial\b/,
    );
    const lens = page.getByRole("combobox", { name: "Servers shown in the results" });
    await expect(lens.locator('option[value="oslo"]')).toHaveText(/^Oslo\b.* · Incomplete$/);
    await lens.evaluate((select: HTMLSelectElement) => {
      select.value = "oslo";
      select.dispatchEvent(new Event("change"));
    });
    await expect(card("upload").locator(".name .status")).toHaveText("Not reached");
    await expect(card("upload").locator(".line .reason")).toHaveText(
      "Left in Download: connection lost",
    );
    const later = saved.result.multiServer.intervals.filter(
      (interval) => interval.reason === "dropout" || interval.stage !== "download",
    );
    expect(later.length).toBeGreaterThan(0);
    for (const interval of later) expect(interval.participants).not.toContain("oslo");
    expect(saved.result.upload?.reportedBytesPerSec).toBeGreaterThan(0);

    await page.evaluate((id) => (location.hash = `/history/${id}`), saved.id);
    await page.reload();
    await expect(
      page.getByRole("combobox", { name: "Servers shown in this result" }),
    ).toContainText("2 of 3 servers");
    expect((await savedResult(page)).result.multiServer).toEqual(saved.result.multiServer);
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
      new MutationObserver(() => phases.push(main.dataset.phase!)).observe(main, {
        attributeFilter: ["data-phase"],
      });
    });
    await killDuringDownload(page, porto, lisbon);
    await expect(page.locator("footer.status .label")).toHaveText("Incomplete", {
      timeout: 20_000,
    });
    expect(await page.evaluate(() => (window as any).phases)).not.toContain("aborted");
    // Download keeps what it measured before both left, enough for a part or too little; Upload had nobody left.
    await expect(page.getByRole("switch", { name: /^Download stage/ })).toHaveAttribute(
      "class",
      /\bchip--(failed|partial)\b/,
    );
    await expect(page.getByRole("switch", { name: /^Upload stage/ })).toContainText("Not reached");
  } finally {
    porto.kill();
    lisbon.kill();
  }
});

test("a run that measured nothing reads Failed with its cause and is kept in History as Incomplete", async (page) => {
  const porto = await spawnPeer("Porto");
  const lisbon = await spawnPeer("Lisbon", catalog(porto.server));
  try {
    await open(page, lisbon.server.url, {
      servers: [{ id: "self", url: lisbon.server.url }, porto.server],
      config: {
        ...long,
        stages: { latency: false, download: true, upload: true, bidirectional: false },
      },
    });
    await ready(page);
    const startedAt = Date.now();
    await runButton(page, "Start test").click();
    await expect(phase(page, "download")).toHaveCount(1, { timeout: 10_000 });
    porto.kill("SIGKILL");
    lisbon.kill("SIGKILL");
    const saved = await savedResult(page, startedAt, 30_000);
    expect(saved.result.outcome).toBe("incomplete");
    await expect(page.locator(".gauge-status.error")).toHaveText("Failed");
    await expect(page.locator("footer.status .label")).toHaveText("Failed");
    await expect(page.locator(".gauge-hint")).toContainText("Download");
    await expect(page.getByRole("switch", { name: /^Download stage/ })).toHaveAttribute(
      "class",
      /\bchip--failed\b/,
    );
  } finally {
    porto.kill();
    lisbon.kill();
  }
});

test("a server that fails between its check and Start is named at once and reads Left at Start", async (page) => {
  const bergen = await spawnPeer("Bergen");
  const servers = [frankfurt, bergen.server];
  const porto = await spawnPeer("Porto", catalog(...servers));
  try {
    await open(page, porto.server.url, {
      servers: [{ id: "self", url: porto.server.url }, ...servers],
      config: long,
    });
    await ready(page);
    bergen.kill("SIGKILL");
    await bergen.exited;
    await page.evaluate(() => {
      const toast = document.querySelector(".phase-toast")!;
      const seen: { text: string; phase: string; ended: number }[] = ((window as any).toasts = []);
      new MutationObserver(() => {
        if (!toast.classList.contains("visible")) return;
        seen.push({
          text: toast.textContent!.replace(/\s+/g, " ").trim(),
          phase: document.getElementById("console")!.dataset.phase!,
          ended: document.querySelectorAll(".chip.chip--complete, .chip.chip--partial").length,
        });
      }).observe(toast, { attributes: true, childList: true, subtree: true, characterData: true });
    });
    const startedAt = Date.now();
    await runButton(page, "Start test").click();
    const saved = await savedResult(page, startedAt, 30_000);
    const [first] = await page.evaluate(() => (window as any).toasts);
    expect(first.text).toContain("Bergen · Start");
    expect(first.ended).toBe(0);
    expect(saved.result.outcome).toBe("partial");
    const lens = page.getByRole("combobox", { name: "Servers shown in the results" });
    await lens.evaluate((select: HTMLSelectElement) => {
      select.value = "bergen";
      select.dispatchEvent(new Event("change"));
    });
    await expect(page.locator('.results .card[data-tone="download"] .line .reason')).toContainText(
      "Left at Start: ",
    );
  } finally {
    bergen.kill();
    porto.kill();
  }
});
