// Temporary gallery capture for one lab profile (GM_LAB_PROFILE), from real runs over its shaped path.
import { open, quit, sleep, WORK } from "./cdp";

const profile = process.env.GM_LAB_PROFILE;
const config = {
  duration: { warmupMs: 1000, latencyMs: 4000, downloadMs: 8000, uploadMs: 8000, bidirectionalMs: 6000 },
};
const home = "https://10.77.1.1:8443/";
type Page = Awaited<ReturnType<typeof open>>;

/** Parks the pointer on empty canvas and waits for every result animation to finish. */
async function still(s: Page, ms = 2500) {
  await s.mouse("mouseMoved", s.width - 4, s.height / 2, { button: "none" });
  await sleep(ms);
  await s.evaluate(
    `Promise.all(document.getAnimations().filter(a => a.effect?.getTiming().iterations !== Infinity && a.playState === "running" && !(a.animationName ?? "").startsWith("sheet-rule")).map(a => a.finished.catch(() => {})))`,
  );
  await sleep(200);
}
async function run(s: Page, timeout = 90_000) {
  const before = await s.phase();
  await s.click(".run-button");
  for (let i = 0; i < 100 && (await s.phase()) === before; i++) await sleep(50);
  if (!(await s.waitPhase("complete", timeout))) throw new Error(`run ended ${await s.phase()}`);
}
async function theme(s: Page, name: string) {
  for (let i = 0; i < 3; i++) {
    const label = await s.evaluate(`document.querySelector('button[aria-label^="Theme"]').getAttribute("aria-label")`);
    if (label.startsWith(`Theme: ${name}`)) return;
    await s.evaluate(`document.querySelector('button[aria-label^="Theme"]').click()`);
    await sleep(700);
  }
}
const panel = (s: Page, label: string) => s.click(`button[aria-label="${label}"]`);
const metrics = (s: Page, width: number, height: number) =>
  s.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 2, mobile: false });

if (profile === "laptop") {
  const s = await open(home, 1600, 1000, { config }, 2);
  await sleep(2500);
  await run(s);
  await still(s, 3500);
  await s.shot("desktop");
  await panel(s, "Settings");
  await still(s);
  await s.shot("settings");
  await panel(s, "Settings");
  await panel(s, "Details");
  await still(s);
  await s.shot("endpoint");
  await panel(s, "Details");
  await still(s, 800);
  await theme(s, "Light");
  await still(s, 1500);
  await s.shot("light");
  await theme(s, "Dark");
  // Both docks on a wide desktop.
  await metrics(s, 1920, 1080);
  await still(s, 800);
  await panel(s, "Settings");
  await panel(s, "Details");
  await still(s);
  await s.shot("workspace");
  await panel(s, "Settings");
  await panel(s, "Details");
  await metrics(s, 1600, 1000);
  await still(s, 800);
  // Two more runs for History, the last with the two-way stage. The first is the hero's desktop, in its upload stage.
  await s.click(".run-button");
  await s.waitPhase("upload", 60_000);
  await s.mouse("mouseMoved", s.width - 4, s.height / 2, { button: "none" });
  await sleep(4600);
  await s.shot("hero-desktop");
  if (!(await s.waitPhase("complete", 60_000))) throw new Error(`hero run ended ${await s.phase()}`);
  await still(s, 1500);
  await panel(s, "Settings");
  await s.evaluate(
    `[...document.querySelectorAll("label")].find(l => /Bidirectional/.test(l.textContent))?.querySelector("input")?.click()`,
  );
  await panel(s, "Settings");
  await still(s, 600);
  await run(s);
  await still(s, 2000);
  await panel(s, "History");
  await sleep(800);
  await s.click("a.result-row");
  await still(s);
  await s.shot("history");
  console.log("laptop", s.errors);
}

if (profile === "multi") {
  const selection = [
    { id: "self", url: home.slice(0, -1) },
    { id: "nas", url: "https://10.77.2.1:8443" },
    { id: "mini", url: "https://10.77.3.1:8443" },
  ];
  const s = await open(home, 1600, 1000, { config, selection }, 2);
  await sleep(3000);
  await run(s);
  await still(s, 3500);
  await s.shot("multi-server");
  // A server that leaves mid-download marks its stage Partial with the reason on the card.
  await s.click(".run-button");
  await s.waitPhase("download", 60_000);
  await sleep(2500);
  process.kill(Number((await Bun.file(`${WORK}/server-3.pid`).text()).trim()), "SIGKILL");
  if (!(await s.waitPhase("complete", 90_000))) throw new Error(`partial run ended ${await s.phase()}`);
  await still(s, 3500);
  await s.shot("partial");
  console.log("multi", s.errors);
}

if (profile === "phone") {
  // A 430 × 932 phone's screen between its status bar and home indicator, as the installed app fills it.
  const s = await open(home, 430, 839, { config }, 3);
  await sleep(2500);
  await run(s);
  await still(s, 3500);
  await s.shot("mobile");
  await panel(s, "History");
  await sleep(800);
  await s.click("a.result-row");
  await still(s);
  await s.shot("mobile-history");
  console.log("phone", s.errors);
}

if (profile === "signin") {
  // The light page at rest, then the dark page while the server checks a password: the check is held open.
  const signin = "https://127.0.0.1:8898/";
  const light = await open(signin, 430, 839, { theme: "light" }, 3);
  await sleep(1500);
  await light.shot("signin");
  await light.close();
  const s = await open(signin, 430, 839, { theme: "dark" }, 3);
  await sleep(1500);
  await s.send("Fetch.enable", { patterns: [{ urlPattern: "*", requestStage: "Request" }] });
  const held = new Promise<string>((resolve) =>
    s.listeners.push((m) => {
      if (m.method !== "Fetch.requestPaused") return;
      if (m.params.request.method === "POST") resolve(m.params.requestId);
      else void s.send("Fetch.continueRequest", { requestId: m.params.requestId });
    }),
  );
  await s.click('input[type="password"]');
  await s.type("gm");
  await s.click('form:has(input[type="password"]) button[type="submit"]');
  await held;
  await sleep(450);
  await s.shot("signin-checking");
  console.log("signin", s.errors);
}
await quit();
process.exit(0);
