import { readHistoryRecord } from "../src/app/history/records";
import type { About as LegalAbout } from "../src/ui/legal/loader";
import {
  baseConfig,
  home,
  open,
  openSettings,
  phase,
  closeSettings,
  ready,
  run,
  runButton,
  savedResult,
} from "./fleet";
import { seriousViolations, expect, settled, test } from "./webview";

test("an HTTP/1.1 and WebSocket run is saved and listed after reload", async (page) => {
  const version = await fetch(`${home.http}/version.json`);
  expect((await version.json()).label).toBe("prod");
  await open(page, home.http, {
    config: {
      transports: {
        throughputTarget: "protocol:http1",
        latencyTarget: "transport:websocket",
      },
      stages: { ...baseConfig.stages, bidirectional: true },
    },
  });
  await ready(page);
  const saved = await run(page);
  expect(readHistoryRecord(saved)).toEqual(saved);
  expect(saved.result.outcome).toBe("complete");
  expect(saved.result.multiServer.failures).toEqual([]);
  const preflight = await fetch(`${home.http}/preflight`);
  const identity = await preflight.json();
  expect(saved.result.multiServer.selection[0]!.name).toBe(identity.server.name);
  expect(saved.engine).toBe(identity.engineVersion);
  for (const stage of ["latency", "download", "upload", "bidirectional"] as const)
    expect(saved.result.stages[stage]).toBe("complete");
  expect(saved.result.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
  expect(saved.result.download?.reportedBytesPerSec).toBeGreaterThan(0);
  expect(saved.result.multiServer.servers[0]!.throughput.transport).toBe("fetch-stream");
  expect(saved.result.multiServer.servers[0]!.latencyTarget?.transport).toBe("websocket");
  expect(saved.result.multiServer.servers[0]!.throughput?.origin).toBe(home.http);

  const readouts = (root: string) =>
    page.evaluate(
      (root) =>
        [...document.querySelectorAll(`${root} .card .headline`)].map((card) =>
          card.textContent!.replace(/\s+/g, " ").trim(),
        ),
      root,
    );
  const live = await readouts(".results");
  expect(live.length).toBeGreaterThan(0);
  const transferred = await page.locator("footer.status .transferred .readout").textContent();
  await page.goto(`${home.http}/#/history/${saved.id}`);
  await expect(page.locator(".detail-pane .card")).toHaveCount(live.length);
  expect(await readouts(".detail-pane")).toEqual(live);
  await expect(page.locator(".detail-pane .head-facts")).toContainText(new RegExp(`Transferred\\s*${transferred}`));
  await page.goto(`${home.http}/#/`);

  const row = page.locator(`a.result-row[data-history-id="${saved.id}"]`);
  await page.getByRole("button", { name: "History", exact: true }).click();
  await expect(page.getByRole("heading", { name: "History" })).toBeVisible();
  await expect(row).toHaveCount(1);
  await page.reload();
  await expect(row).toHaveCount(1);
});

test("the idle track draws one bar per reply bucket and grows as each closes", async (page) => {
  await open(page, home.http, {
    config: {
      stages: { ...baseConfig.stages, download: false, upload: false },
      duration: { ...baseConfig.duration, latencyMs: 2000 },
    },
  });
  await ready(page);
  await page.evaluate(() => {
    const live = new Set<number>();
    const sample = () => {
      const phase = document.getElementById("console")!.dataset.phase;
      const bars = document.querySelectorAll('.results .card[data-tone="latency"] .track line.reply').length;
      if (phase === "latency") live.add(bars);
      Object.assign(window, { live: live.size, bars });
      if (phase !== "complete") requestAnimationFrame(sample);
    };
    requestAnimationFrame(sample);
  });
  await run(page);
  const { live, bars } = await page.evaluate(() => {
    const { live, bars } = window as unknown as Record<string, number>;
    return { live, bars };
  });
  // Reply-driven probes fill 200 ms buckets: ten over two seconds, and the replies of the stage's last moment.
  expect(bars).toBeGreaterThanOrEqual(9);
  expect(bars).toBeLessThanOrEqual(11);
  expect(live).toBeGreaterThanOrEqual(5);
});

// Every page reports a policy violation as an error, so a run that trips Trusted Types fails whichever test made it.
test("the page requires Trusted Types and refuses script it did not make", async (page) => {
  const policy = (await fetch(`${home.http}/`)).headers.get("content-security-policy");
  expect(policy).toContain("; require-trusted-types-for 'script'; trusted-types graphite-meter; ");
  await open(page);
  await ready(page);
  const refusals = await page.evaluate(() =>
    [() => new Worker("/assets/elsewhere.js"), () => (document.body.innerHTML = "<b></b>")].map((sink) => {
      try {
        sink();
        return "admitted";
      } catch (error) {
        return (error as Error).name;
      }
    }),
  );
  expect(refusals).toEqual(["TypeError", "TypeError"]);
  // The guard reports each refusal; Firefox's console reports them too.
  const reports = () => page.errors.filter((error) => error.includes("CSP violation: require-trusted-types-for"));
  await expect.poll(() => reports().length).toBe(2);
  expect(page.errors.every((error) => error.includes("require-trusted-types-for"))).toBe(true);
  page.errors.length = 0;
});

test("stopping during download freezes elapsed time and a rerun completes", async (page) => {
  await open(page, home.url, {
    config: { duration: { ...baseConfig.duration, downloadMs: 1500 } },
  });
  await ready(page);
  await runButton(page, "Start test").click();
  await expect(phase(page, "download")).toHaveCount(1, { timeout: 10_000 });
  await runButton(page, "Stop test").click();
  await expect(phase(page, "aborted")).toHaveCount(1);
  const elapsed = page.locator(".elapsed");
  const frozen = await elapsed.textContent();
  // A running clock would redraw its 0.1 s readout within eight frames.
  await page.evaluate(async () => {
    for (let frame = 0; frame < 8; frame++) await new Promise((resolve) => requestAnimationFrame(resolve));
  });
  expect(await elapsed.textContent()).toBe(frozen);
  const saved = await run(page);
  expect(saved.result.outcome).toBe("complete");
});

test("a completed run's arcs draw each result's share of the ring", async (page) => {
  await open(page);
  await ready(page);
  await run(page);
  await page.evaluate(settled);
  const arcs = await page.locator(".result-arc").all((paths: SVGPathElement[]) =>
    paths.map((path) => ({
      drawn: parseFloat(getComputedStyle(path).strokeDasharray),
      wanted: path.getTotalLength() * Number(getComputedStyle(path).getPropertyValue("--fraction")),
    })),
  );
  expect(arcs.length).toBeGreaterThan(0);
  // A browser that drops the dash array draws the whole ring.
  for (const arc of arcs) expect(Math.abs(arc.drawn - arc.wanted)).toBeLessThan(1);
});

test.chrome("reduced motion still updates every readout and completes", async (page) => {
  await page.cdp("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-reduced-motion", value: "reduce" }],
  });
  await open(page);
  await ready(page);
  await runButton(page, "Start test").click();
  await expect(phase(page, "download")).toHaveCount(1, { timeout: 10_000 });
  const elapsed = page.locator(".elapsed .readout");
  const first = await elapsed.textContent();
  await expect.poll(async () => (await elapsed.textContent()) !== first).toBe(true);
  await expect(page.locator(".gauge-value")).toHaveText(/\d/);
  await expect(page.locator('.results .card[data-tone="download"] .graph')).toBeVisible();
  const saved = await savedResult(page);
  expect(saved.result.outcome).toBe("complete");
  await expect(page.locator('.results .card[data-tone="download"] .facts')).toContainText("Peak");
});

test.chrome("a run in a hidden tab completes and saves", async (page) => {
  await open(page);
  await ready(page);
  // Closing Settings leaves its toggle's wash fading on the compositor; Chrome freezes a page minimized mid-fade.
  await page.evaluate(settled);
  const { windowId } = await page.cdp("Browser.getWindowForTarget");
  const bounds = (windowState: string) => page.cdp("Browser.setWindowBounds", { windowId, bounds: { windowState } });
  const startedAt = Date.now();
  await runButton(page, "Start test").click();
  await bounds("minimized");
  expect(await page.evaluate(() => document.visibilityState)).toBe("hidden");
  const saved = await savedResult(page, startedAt, 20_000);
  await bounds("normal");
  expect(saved.result.outcome).toBe("complete");
  await expect(phase(page, "complete")).toHaveCount(1);
});

const viewports = [
  [390, 844],
  [768, 1024],
  [1280, 800],
  [1920, 1080],
] as const;

test("a completed run fits every layout and theme without serious violations", async (page) => {
  await open(page);
  await ready(page);
  await run(page);
  expect(await page.locator(".phase-toast.visible").state()).toEqual([]);
  for (const [width, height] of viewports)
    for (const scheme of ["light", "dark"]) {
      await page.setViewportSize({ width, height });
      // The theme as the app stamps it; contrast waits for fades to settle (seriousViolations).
      await page.evaluate((theme) => document.documentElement.setAttribute("data-theme", theme), scheme);
      await openSettings(page);
      const layout = () =>
        page.evaluate(() => {
          const overflow = [...document.querySelectorAll(".panel-body, .stage")].filter(
            (el) => el.scrollWidth > el.clientWidth + 1,
          );
          const stage = document.querySelector(".gauge-panel .dial")!.getBoundingClientRect();
          const gauge = document.querySelector(".gauge-face")!.getBoundingClientRect();
          return {
            width: innerWidth,
            page: document.documentElement.scrollWidth <= innerWidth,
            overflow: overflow.map((el) => el.className),
            gauge:
              gauge.left >= stage.left - 1 &&
              gauge.right <= stage.right + 1 &&
              gauge.top >= stage.top - 1 &&
              gauge.bottom <= stage.bottom + 1,
          };
        });
      await expect.poll(layout).toEqual({ width, page: true, overflow: [], gauge: true });
      expect(await seriousViolations(page)).toEqual([]);
      await closeSettings(page);
    }
  await page.getByRole("button", { name: "Details" }).click();
  const about = page.getByRole("button", { name: "About & legal" });
  await about.click();
  const dialog = page.getByRole("dialog", { name: "About & legal" });
  expect(await dialog.evaluate((el) => el.matches(":modal"))).toBe(true);
  await expect(dialog).toContainText("AGPL-3.0-or-later");
  // The server lists the crates it ships, under the version it reports.
  const identity = await (await fetch(`${home.http}/preflight`)).json();
  const legal: LegalAbout & { sourceVersion: string } = await (await fetch(`${home.http}/legal/about.json`)).json();
  expect(new Set(legal.components.map((c) => c.ecosystem)).has("cargo")).toBe(true);
  expect(identity.engineVersion).toBe(legal.sourceVersion);
  await expect(dialog).toContainText("Rust crates");
  // Every component reads one way: where it comes from (a fork's upstream), then a modified one's shipped copy and
  // changes.
  const links = legal.components.flatMap((component) => {
    expect(component.links.length).toBeGreaterThan(0);
    for (const link of component.links) expect(new URL(link.url).protocol).toMatch(/^https?:$/);
    const url = (label: string) => component.links.find((link) => link.label === label)?.url;
    const [source, upstream, changes] = ["Source", "Upstream", "Changes"].map(url);
    // A link's name leads with its visible text, then says whose it is.
    return [
      { name: `, source of ${component.name}`, href: upstream ?? source },
      ...(component.modified && upstream ? [{ name: `, shipped source of ${component.name}`, href: source }] : []),
      ...(component.modified && changes ? [{ name: `changes to ${component.name}`, href: changes }] : []),
    ];
  });
  const shown = await dialog.evaluate((el: HTMLElement) =>
    Array.from(el.querySelectorAll(".where a"), (link) => ({
      name: link.textContent!.replace(/\s+/g, " ").trim(),
      href: link.getAttribute("href"),
    })),
  );
  expect(
    shown.map(({ name, href }, i) => ({
      name: name.endsWith(links[i]?.name ?? "\0") ? links[i]!.name : name,
      href,
    })),
  ).toEqual(links);
  const fork = legal.components.find((c) => c.name === "noq")!;
  expect(fork.links.map((l) => l.label)).toEqual(["Source", "Upstream", "Changes"]);
  expect(fork.links[0]!.url).toMatch(/^https:\/\/github\.com\/zR-JB\/noq\/tree\/[0-9a-f]{40}$/);
  expect(fork.links[1]!.url).toMatch(/^https:\/\/github\.com\/n0-computer\/noq\/tree\/[0-9a-f]{40}$/);
  expect(await seriousViolations(page, '[role="dialog"]')).toEqual([]);
  await page.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(about).toBeFocused();
});

test.chrome("a run logs nothing to the console, rendering errors included", async (page) => {
  await open(page);
  await ready(page);
  await page.cdp("Log.enable");
  const logged: string[] = [];
  await page.onCdp("Log.entryAdded", (event: any) => {
    const entry = event.data?.entry;
    if (entry?.level === "error" || entry?.level === "warning") logged.push(`${entry.source}: ${entry.text}`);
  });
  await run(page);
  expect(logged).toEqual([]);
});
