// Renders the README screenshots into docs/assets in Chrome; `mise run screenshots` runs it.
import type { BunPlugin } from "bun";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { bundles } from "../../bundle";
import { DEFAULT_CONFIG } from "../../src/app/preferences";
import {
  catalog,
  HISTORY_DB,
  open,
  openSettings,
  phase,
  runButton,
  spawnPeer,
} from "../../e2e/fleet";
import { expect, Page, settled } from "../../e2e/webview";
import type { Scenario, Spread } from "./fakes";
import { decode, encode } from "./png";

type Theme = "light" | "dark";
const THEMES: Theme[] = ["light", "dark"];
const version = process.env.GM_SCREENSHOT_VERSION;
const cellsPath = process.env.GM_SCREENSHOT_TERMINAL;
if (!version || !cellsPath)
  throw new Error("GM_SCREENSHOT_VERSION and GM_SCREENSHOT_TERMINAL are required");
const assets = resolve("../docs/assets");
// History reads its relative times against this moment; every page starts its clock there.
const NOW = Date.UTC(2026, 9, 17, 9, 13, 30);
const DESKTOP = { width: 1440, height: 900 };
const PHONE = { width: 393, height: 852 };
const PHONE_AGENT =
  "Mozilla/5.0 (Linux; Android 16; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Mobile Safari/537.36";

/** The app, built with the fakes in place of the lanes, the probes, the run's clock and preflight timing. */
async function fakeApp() {
  const src = resolve("src");
  const fakes = resolve(import.meta.dir, "fakes.ts");
  const swaps: Record<string, Record<string, string>> = {
    [join(src, "app/app.ts")]: { "../engine/lanes/probes": fakes, "../engine/lanes/stage": fakes },
    [join(src, "engine/run/run.ts")]: { "./clock": fakes },
    [join(src, "engine/net/connection.ts")]: { "./checks": resolve(import.meta.dir, "checks.ts") },
  };
  let swapped = 0;
  const plugin: BunPlugin = {
    name: "screenshot fakes",
    setup(build) {
      build.onResolve(
        { filter: /^\.\.?\/(engine\/lanes\/(probes|stage)|clock|checks)$/ },
        ({ path, importer }) => {
          const swap = swaps[importer]?.[path];
          if (swap) swapped++;
          return swap ? { path: swap } : undefined;
        },
      );
    },
  };
  const outdir = await mkdtemp(join(tmpdir(), "gm-screenshots-"));
  const revision = Bun.spawnSync(["git", "rev-parse", "--short", "HEAD"]).stdout.toString().trim();
  const { workers, app } = await bundles(outdir, { profile: "prod", version: version!, revision }, [
    plugin,
  ]);
  if (swapped !== 4)
    throw new Error(`the fakes replaced ${swapped} of 4 imports; an importer above changed them`);
  const files = new Map<string, Uint8Array>();
  for (const output of workers.outputs)
    files.set(`/assets/${basename(output.path)}`, await Bun.file(output.path).bytes());
  const main = app.outputs.find((output) => output.kind === "entry-point")!;
  return { files, main: await Bun.file(main.path).bytes() };
}

const build = await fakeApp();
// Servers of its own: the fleet's carry a location the README's servers never had.
const unplaced = { GM_SERVER_LOCATION: "" };
const [frankfurt, amsterdam] = await Promise.all([
  spawnPeer("Frankfurt", unplaced),
  spawnPeer("Amsterdam", unplaced),
]);
const home = await spawnPeer("Home", {
  ...unplaced,
  ...catalog(frankfurt.server, amsterdam.server),
});
const servers = [home, frankfurt, amsterdam];
const rtt = Object.fromEntries(
  servers.flatMap(({ server }, index) =>
    [server.http, server.url, server.h2, server.h3].map((url) => [
      new URL(url).origin,
      [10.1, 11.7, 15.8][index],
    ]),
  ),
);

/** Holds the page's wall clock at NOW, or wherever __gmSetNow moves it, and gives the fakes the round trips. */
function pin({ now, rtt }: { now: number; rtt: Record<string, number> }) {
  const Real = Date;
  let [base, anchor] = [now, Real.now()];
  const at = () => base + Real.now() - anchor;
  globalThis.Date = class extends Real {
    constructor(...args: unknown[]) {
      super(...((args.length ? args : [at()]) as [number]));
    }
    static override now() {
      return at();
    }
  } as DateConstructor;
  Reflect.set(
    globalThis,
    "__gmSetNow",
    (moment: number) => ([base, anchor] = [moment, Real.now()]),
  );
  Reflect.set(globalThis, "__gmScenario", { epoch: now, rtt });
}

const setNow = (page: Page, moment: number) =>
  page.evaluate((moment) => Reflect.get(globalThis, "__gmSetNow")(moment), moment);

async function device(size: { width: number; height: number }, phone: boolean) {
  const page = new Page({ monitorDisplay: false });
  await page.onCdp("Fetch.requestPaused", (event) => {
    const { requestId, request } = (
      event as { data: { requestId: string; request: { url: string } } }
    ).data;
    const path = new URL(request.url).pathname;
    const body = /^\/assets\/main-[^/]+\.js$/.test(path) ? build.main : build.files.get(path);
    const answer = body
      ? page.cdp("Fetch.fulfillRequest", {
          requestId,
          responseCode: 200,
          responseHeaders: [{ name: "Content-Type", value: "text/javascript; charset=utf-8" }],
          body: Buffer.from(body).toString("base64"),
        })
      : page.cdp("Fetch.continueRequest", { requestId });
    answer.catch((error) => console.error(`serving ${path}: ${error.message}`));
  });
  await page.cdp("Fetch.enable", { patterns: [{ urlPattern: "*/assets/*.js" }] });
  await page.cdp("Emulation.setDeviceMetricsOverride", {
    ...size,
    deviceScaleFactor: 2,
    mobile: phone,
  });
  if (phone) {
    await page.cdp("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 5 });
    await page.cdp("Emulation.setUserAgentOverride", { userAgent: PHONE_AGENT });
  }
  await page.cdp("Emulation.setTimezoneOverride", { timezoneId: "UTC" });
  await page.cdp("Emulation.setLocaleOverride", { locale: "en-US" });
  await media(page, "light");
  await page.addInitScript(pin, { now: NOW, rtt });
  return page;
}

const media = (page: Page, theme: Theme) =>
  page.cdp("Emulation.setEmulatedMedia", {
    features: [
      { name: "prefers-color-scheme", value: theme },
      { name: "prefers-reduced-motion", value: "reduce" },
    ],
  });

const spread = (median: number, jitter: number, min: number, max: number): Spread => ({
  median,
  jitter,
  min,
  max,
});
const mbit = (rate: number) => (rate * 1e6) / 8;

/** One run of `scenario` that ends at `completedAt`, through to its saved result. */
async function play(page: Page, completedAt: number, scenario: Omit<Scenario, "epoch" | "rtt">) {
  await setNow(page, completedAt);
  const patch = { epoch: completedAt - 20_000, ...scenario };
  await page.evaluate(
    (patch) => Object.assign(Reflect.get(globalThis, "__gmScenario"), patch),
    patch,
  );
  const saved = () => page.evaluate(countResults, HISTORY_DB);
  const before = await saved();
  await runButton(page, /^(Start test|Run again)$/).click();
  await expect(phase(page, "complete")).toHaveCount(1, { timeout: 20_000 });
  await expect.poll(saved).toBe(before + 1);
}

async function countResults(db: typeof HISTORY_DB) {
  if (!(await indexedDB.databases()).some(({ name }) => name === db.name)) return 0;
  return new Promise<number>((resolve, reject) => {
    const opening = indexedDB.open(db.name);
    opening.onerror = () => reject(opening.error);
    opening.onsuccess = () => {
      const database = opening.result;
      const count = database.transaction(db.resultsStore).objectStore(db.resultsStore).count();
      count.onsuccess = () => resolve((database.close(), count.result));
      count.onerror = () => reject(count.error);
    };
  });
}

async function shoot(page: Page): Promise<Record<Theme, string>> {
  const shots = {} as Record<Theme, string>;
  for (const theme of THEMES) {
    await media(page, theme);
    await page.evaluate(async () => {
      await document.fonts.ready;
      for (let frame = 0; frame < 3; frame++)
        await new Promise((done) => requestAnimationFrame(done));
    });
    await page.evaluate(settled);
    shots[theme] = (
      await page.cdp<{ data: string }>("Page.captureScreenshot", { format: "png" })
    ).data;
  }
  return shots;
}

async function reload(page: Page) {
  await page.reload();
  await expect(page.locator("#console")).toHaveCount(1);
}

const desktop = await device(DESKTOP, false);
await desktop.clearStorage([home.server.http, home.server.url]);
await open(desktop, home.server.url, { config: DEFAULT_CONFIG });
await play(desktop, NOW - 300_000, {
  down: 9.39e9 / 8,
  up: 9.3956e9 / 8,
  wander: 0.04,
  overshoot: 0.06,
  idle: spread(0.4, 0.06, 0.2, 1.53),
  loaded: { download: spread(1.7, 0.03, 1.5, 2), upload: spread(1.7, 0.2, 1.3, 2.2) },
  replies: { idle: 2481, loaded: 4 },
});
const hero = await shoot(desktop);
const errors = [...desktop.errors];
await desktop.close();

// History's four results, the last of them the hero's phone run, then three servers for Settings.
const phone = await device(PHONE, true);
await phone.clearStorage([home.server.http, home.server.url]);
await open(phone, home.server.url, { config: DEFAULT_CONFIG });
/** A phone run: when it ended before NOW, its rates in Mbit/s over all `servers`, and its latency in ms. */
interface PhoneRun {
  ago: number;
  down: number;
  up: number;
  servers: number;
  idle: Spread;
  loaded: [number, number];
}
const phoneRun = ({ ago, down, up, servers, idle, loaded: [loadedDown, loadedUp] }: PhoneRun) =>
  play(phone, NOW - ago, {
    down: mbit(down / servers),
    up: mbit(up / servers),
    wander: 0.07,
    overshoot: 0,
    idle,
    loaded: {
      download: spread(loadedDown, 0.3, loadedDown - 2, loadedDown + 4),
      upload: spread(loadedUp, 0.3, loadedUp - 2, loadedUp + 4),
    },
    replies: { idle: 365, loaded: 4 },
  });
const runs: PhoneRun[] = [
  {
    ago: 95_000,
    down: 527.4,
    up: 315.19,
    servers: 1,
    idle: spread(4.9, 0.32, 4.1, 6.2),
    loaded: [21.8, 16.6],
  },
  {
    ago: 75_000,
    down: 865.5,
    up: 572.94,
    servers: 1,
    idle: spread(3.7, 0.3, 3.1, 4.8),
    loaded: [15.6, 13.4],
  },
  {
    ago: 40_000,
    down: 1196,
    up: 863.82,
    servers: 1,
    idle: spread(2.7, 0.2, 2.2, 3.4),
    loaded: [11.8, 12.1],
  },
];
for (const [index, spec] of runs.entries()) {
  if (index) await reload(phone);
  await phoneRun(spec);
}
const phoneResult = await shoot(phone);
await phone.evaluate(
  (servers) => localStorage.setItem("graphite-meter:server-selection:v1", JSON.stringify(servers)),
  [{ id: "self", url: home.server.url }, frankfurt.server, amsterdam.server].map(({ id, url }) => ({
    id,
    url,
  })),
);
await reload(phone);
await phoneRun({
  ago: 20_000,
  down: 1193,
  up: 852.02,
  servers: 3,
  idle: spread(3, 1, 1.6, 7.2),
  loaded: [11.9, 14.9],
});
await setNow(phone, NOW);
await phone.goto(`${home.server.url}/#/history`);
await expect(phone.locator(".result-row")).toHaveCount(4);
const history = await shoot(phone);
await phone.goto(`${home.server.url}/#/`);
const settings = await openSettings(phone);
await expect(settings.locator('[data-readiness="verified"]')).toBeVisible({ timeout: 15_000 });
const settingsShots = await shoot(phone);
errors.push(...phone.errors);
await phone.close();
for (const { kill } of servers) kill();
if (errors.length) throw new Error(`the app reported errors:\n${errors.join("\n")}`);

const PALETTE = {
  light: {
    app: "#f1f2f5",
    edge: "#cdd3dc",
    line: "#d3d8df",
    backdrop: ["#e2e6ed", "#cdd5e0"],
    shadow: 0.22,
  },
  dark: {
    app: "#0d1013",
    edge: "#2c323b",
    line: "#2a2f37",
    backdrop: ["#1e242e", "#11151c"],
    shadow: 0.6,
  },
};
const png = (data: string) => `data:image/png;base64,${data}`;

/** The desktop with the phone overlapping its lower-right corner, over a soft backdrop. */
function heroHtml(theme: Theme) {
  const c = PALETTE[theme];
  return `<style>
    body { margin: 0; }
    .canvas { position: relative; width: 3480px; height: 2300px; border-radius: 52px; overflow: hidden;
      background: linear-gradient(${c.backdrop[0]}, ${c.backdrop[1]}); }
    .desk { position: absolute; left: 150px; top: 150px; width: 2880px; height: 1800px; border-radius: 30px;
      overflow: hidden; box-shadow: 0 24px 100px rgb(0 0 0 / ${c.shadow}); }
    .desk::after { content: ""; position: absolute; inset: 0; border-radius: 30px; box-shadow: inset 0 0 0 3px ${c.edge}; }
    .phone { position: absolute; left: 2641px; top: 727px; width: 689px; height: 1423px; border-radius: 125px;
      background: #16181c; box-shadow: inset 0 0 0 1px #16181c, inset 0 0 0 5px #464b54,
      0 28px 90px rgb(0 0 0 / ${Math.min(1, c.shadow * 1.6)}); }
    .screen { position: absolute; left: 30px; top: 30px; width: 629px; height: 1363px; border-radius: 95px;
      overflow: hidden; background: ${c.app}; }
    .crop { position: absolute; top: 32px; width: 629px; height: 1299px; overflow: hidden; }
    img { display: block; }
  </style>
  <div class="canvas">
    <div class="desk"><img src="${png(hero[theme])}" width="2880" height="1800"></div>
    <div class="phone"><div class="screen"><div class="crop">
      <img src="${png(phoneResult[theme])}" width="629"></div></div></div>
  </div>`;
}

/** Settings and History side by side, each screen's top inset in its rim, fading out at the bottom. */
function phonesHtml(theme: Theme) {
  const c = PALETTE[theme];
  const panel = (shot: string) =>
    `<div class="panel"><div class="crop"><img src="${png(shot)}" width="786"></div></div>`;
  return `<style>
    body { margin: 0; display: flex; gap: 72px; width: 1772px; }
    .panel { position: relative; width: 850px; height: 1364px; border-radius: 88px; overflow: hidden;
      background: ${c.app}; mask-image: linear-gradient(black calc(100% - 260px), transparent calc(100% - 2px)); }
    .panel::after { content: ""; position: absolute; inset: 0; border-radius: 88px; box-shadow: inset 0 0 0 2px ${c.line}; }
    .crop { margin: 32px; height: 1300px; overflow: hidden; }
    img { display: block; }
  </style>${panel(settingsShots[theme])}${panel(history[theme])}`;
}

function terminalHtml(theme: Theme, canvas: string) {
  return `<style>
    body { margin: 0; }
    .rim { position: relative; width: 2240px; height: 1680px; background: ${canvas}; }
    .rim::after { content: ""; position: absolute; inset: 0; border-radius: 28px;
      box-shadow: inset 0 0 0 2px ${PALETTE[theme].line}; }
    canvas { position: absolute; left: 40px; top: 40px; }
  </style><div class="rim"><canvas width="2160" height="1600"></canvas></div>`;
}

type Cell = [string, string, string, boolean];
/** Draws the console's cells as a 2x terminal would: 18x40 px cells, IBM Plex Mono, line, block and braille glyphs drawn, not from a font. */
async function paintTerminal({
  rows,
  canvas,
  fonts,
}: {
  rows: Cell[][];
  canvas: string;
  fonts: Record<string, string>;
}) {
  const faces = [
    new FontFace("Plex Mono", `url(${fonts.mono500})`, { weight: "500" }),
    new FontFace("Plex Mono", `url(${fonts.mono600})`, { weight: "600" }),
    new FontFace("Plex Pi", `url(${fonts.pi})`, { weight: "100 700" }),
  ];
  for (const face of faces) document.fonts.add(await face.load());
  const [cw, ch] = [18, 40];
  const g = document.querySelector("canvas")!.getContext("2d")!;
  g.fillStyle = canvas;
  g.fillRect(0, 0, cw * rows[0]!.length, ch * rows.length);
  // Each line glyph's arms toward left, right, up and down: 1 light, 2 heavy.
  const LINES = Object.fromEntries(
    "─1100 ━2200 │0011 ┃0022 ┌0101 ┐1001 └0110 ┘1010 ┏0202 ┓2002 ┗0220 ┛2020 ├0111 ┤1011 ┬1101 ┴1110 ┼1111 ┣0222 ┫2022 ┳2202 ┻2220 ╋2222 ╴1000 ╵0010 ╶0100 ╷0001 ╸2000 ╹0020 ╺0200 ╻0002"
      .split(" ")
      .map((entry) => [entry[0]!, [...entry.slice(1)].map(Number)]),
  );
  const thickness = (weight: number) => (weight === 2 ? 5 : 2);
  const DOTS = [0, 1, 2, 4, 5, 6, 3, 7].map((cell) => [cell >> 2, cell & 3]);
  for (const [y, row] of rows.entries())
    row.forEach(([symbol, fg, bg, bold], x) => {
      const [left, top] = [x * cw, y * ch];
      if (bg !== canvas) {
        g.fillStyle = bg;
        g.fillRect(left, top, cw, ch);
      }
      if (symbol === " ") return;
      g.fillStyle = fg;
      const code = symbol.codePointAt(0)!;
      const arms = LINES[symbol];
      if (arms) {
        const [l, r, u, d] = arms as [number, number, number, number];
        // Each arm runs past the centre by half the crossing arm, so corners close.
        const [across, along] = [Math.max(u, d), Math.max(l, r)].map(
          (weight) => Math.floor(thickness(weight) / 2) + 1,
        );
        const [cx, cy] = [left + cw / 2, top + ch / 2];
        const row = (weight: number) => top + Math.floor((ch - thickness(weight)) / 2);
        const column = (weight: number) => left + Math.floor((cw - thickness(weight)) / 2);
        if (l) g.fillRect(left, row(l), cw / 2 + across!, thickness(l));
        if (r) g.fillRect(cx - across!, row(r), cw / 2 + across!, thickness(r));
        if (u) g.fillRect(column(u), top, thickness(u), ch / 2 + along!);
        if (d) g.fillRect(column(d), cy - along!, thickness(d), ch / 2 + along!);
      } else if (symbol === "┊") {
        for (let dash = 0; dash < 4; dash++)
          g.fillRect(left + (cw - 2) / 2, top + dash * 10 + 2.5, 2, 5);
      } else if (code >= 0x2800 && code <= 0x28ff) {
        DOTS.forEach(([column, line], bit) => {
          if (!((code - 0x2800) & (1 << bit))) return;
          g.beginPath();
          g.arc(
            left + (cw * (1 + 2 * column!)) / 4,
            top + (ch * (1 + 2 * line!)) / 8,
            cw / 8,
            0,
            2 * Math.PI,
          );
          g.fill();
        });
      } else if (symbol === "●" || symbol === "◆") {
        const [cx, cy, size] = [left + cw / 2, top + ch / 2, cw * 0.36];
        g.beginPath();
        if (symbol === "●") g.arc(cx, cy, size, 0, 2 * Math.PI);
        else
          for (const [dx, dy] of [
            [0, -1],
            [1, 0],
            [0, 1],
            [-1, 0],
          ])
            g.lineTo(cx + dx! * size, cy + dy! * size);
        g.fill();
      } else {
        g.font = `${bold ? 600 : 500} 30px "Plex Mono", "Plex Pi"`;
        g.fillText(symbol, left + (cw - g.measureText(symbol).width) / 2, top + 31);
      }
    });
}

/** Renders `html` at `width` px wide from its `size` in CSS px, and writes it as a 256-color PNG. */
async function render(
  page: Page,
  html: string,
  size: { width: number; height: number },
  width: number,
  name: string,
  after?: () => Promise<unknown>,
) {
  const scale = width / size.width;
  await page.cdp("Emulation.setDeviceMetricsOverride", {
    ...size,
    deviceScaleFactor: scale,
    mobile: false,
  });
  await page.cdp("Emulation.setDefaultBackgroundColorOverride", {
    color: { r: 0, g: 0, b: 0, a: 0 },
  });
  const { frameTree } = await page.cdp<{ frameTree: { frame: { id: string } } }>(
    "Page.getFrameTree",
  );
  await page.cdp("Page.setDocumentContent", { frameId: frameTree.frame.id, html });
  await page.evaluate(() => Promise.all([...document.images].map((image) => image.decode())));
  await after?.();
  const { data } = await page.cdp<{ data: string }>("Page.captureScreenshot", { format: "png" });
  const image = decode(Buffer.from(data, "base64"));
  const height = Math.round(size.height * scale);
  if (image.width !== width || image.height < height)
    throw new Error(`${name} came out ${image.width}x${image.height}`);
  await Bun.write(
    join(assets, name),
    encode({ width, height, data: image.data.subarray(0, width * height * 4) }),
  );
}

const fontFile = async (name: string) =>
  `data:font/woff2;base64,${Buffer.from(await Bun.file(resolve("public/fonts", name)).arrayBuffer()).toString("base64")}`;
const fonts = {
  mono500: await fontFile("ibm-plex-mono-500-latin1.woff2"),
  mono600: await fontFile("ibm-plex-mono-600-latin1.woff2"),
  pi: await fontFile("ibm-plex-sans-var-pi.woff2"),
};
const terminal: Record<Theme, { canvas: string; rows: Cell[][] }> =
  await Bun.file(cellsPath).json();
const studio = new Page({ monitorDisplay: false });
await studio.goto("about:blank");
for (const theme of THEMES) {
  await render(studio, heroHtml(theme), { width: 3480, height: 2300 }, 2400, `hero-${theme}.png`);
  await render(
    studio,
    phonesHtml(theme),
    { width: 1772, height: 1364 },
    1400,
    `phones-${theme}.png`,
  );
  const { canvas, rows } = terminal[theme];
  await render(
    studio,
    terminalHtml(theme, canvas),
    { width: 2240, height: 1680 },
    2000,
    `terminal-${theme}.png`,
    () => studio.evaluate(paintTerminal, { rows, canvas, fonts }),
  );
}
await studio.close();
