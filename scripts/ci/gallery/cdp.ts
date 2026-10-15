// Temporary gallery driver: headless Chrome over its DevTools Protocol, with a seeded config, input and screenshots.
import { spawn } from "node:child_process";

export const WORK = `${import.meta.dir}/work`;
export const OUT = `${import.meta.dir}/out`;
const PORT = 9555;
export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function ensureChrome() {
  try {
    await fetch(`http://127.0.0.1:${PORT}/json/version`);
    return;
  } catch {}
  const chrome = spawn(
    process.env.BUN_CHROME_PATH ?? "chrome",
    [
      "--headless=new",
      `--remote-debugging-port=${PORT}`,
      `--user-data-dir=${WORK}/chrome-${process.env.GM_LAB_PROFILE ?? "page"}`,
      "--no-sandbox",
      "--disable-dev-shm-usage",
      "--no-first-run",
      "--disable-gpu-vsync",
      "--hide-scrollbars",
      "--ignore-certificate-errors",
      "--force-color-profile=srgb",
      "about:blank",
    ],
    { detached: true, stdio: "ignore" },
  );
  chrome.unref();
  for (let i = 0; i < 100; i++) {
    await sleep(100);
    try {
      await fetch(`http://127.0.0.1:${PORT}/json/version`);
      return;
    } catch {}
  }
  throw new Error("chrome did not start");
}

/** Closes the browser this profile started. */
export async function quit() {
  const { webSocketDebuggerUrl } = (await (await fetch(`http://127.0.0.1:${PORT}/json/version`)).json()) as {
    webSocketDebuggerUrl: string;
  };
  const ws = new WebSocket(webSocketDebuggerUrl);
  await new Promise((r) => (ws.onopen = r));
  ws.send(JSON.stringify({ id: 1, method: "Browser.close" }));
  await sleep(1000);
}

export type Seed = {
  theme?: "dark" | "light";
  config?: Record<string, unknown>;
  selection?: { id: string; url: string }[];
};

export async function open(url: string, width: number, height: number, seed: Seed = {}, dpr = 1) {
  await ensureChrome();
  const res = await fetch(`http://127.0.0.1:${PORT}/json/new?about:blank`, { method: "PUT" });
  const target = (await res.json()) as { id: string; webSocketDebuggerUrl: string };
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  let id = 0;
  const pending = new Map<number, (v: any) => void>();
  const listeners: ((m: any) => void)[] = [];
  ws.onmessage = (ev) => {
    const msg = JSON.parse(String(ev.data));
    if (msg.id && pending.has(msg.id)) {
      pending.get(msg.id)!(msg);
      pending.delete(msg.id);
    } else listeners.forEach((l) => l(msg));
  };
  await new Promise((r) => (ws.onopen = r));
  const send = (method: string, params: any = {}) =>
    new Promise<any>((resolve) => {
      const n = ++id;
      pending.set(n, resolve);
      ws.send(JSON.stringify({ id: n, method, params }));
    });
  const evaluate = async (expression: string) => {
    const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (r.result?.exceptionDetails) throw new Error(JSON.stringify(r.result.exceptionDetails).slice(0, 400));
    return r.result?.result?.value;
  };
  const errors: string[] = [];
  listeners.push((m) => {
    if (m.method === "Runtime.exceptionThrown") errors.push(JSON.stringify(m.params.exceptionDetails).slice(0, 300));
    if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error")
      errors.push(m.params.args.map((a: any) => a.value ?? a.description).join(" ").slice(0, 300));
  });
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: dpr, mobile: width < 600 });
  const theme = seed.theme ?? "dark";
  await send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: theme }] });
  const stored = {
    theme,
    resultHistoryPreference: "enabled",
    config: { stages: { latency: true, download: true, upload: true, bidirectional: false }, ...seed.config },
  };
  const selection = seed.selection
    ? `localStorage.setItem("graphite-meter:server-selection:v1", ${JSON.stringify(JSON.stringify(seed.selection))});`
    : `localStorage.removeItem("graphite-meter:server-selection:v1");`;
  await send("Page.addScriptToEvaluateOnNewDocument", {
    source: `if (!sessionStorage.getItem("seeded")) { localStorage.setItem("graphite-meter:v1", ${JSON.stringify(
      JSON.stringify(stored),
    )}); ${selection} sessionStorage.setItem("seeded", "1"); }`,
  });
  await send("Page.navigate", { url });
  await sleep(1500);

  const center = async (sel: string) =>
    (await evaluate(
      `(() => { const el = document.querySelector(${JSON.stringify(sel)}); if (!el) return null; const r = el.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; })()`,
    )) as [number, number] | null;
  const mouse = (type: string, x: number, y: number, extra: any = {}) =>
    send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1, ...extra });

  const s = {
    send,
    evaluate,
    errors,
    listeners,
    width,
    height,
    async shot(name: string) {
      const r = await send("Page.captureScreenshot", { format: "png" });
      await Bun.write(`${OUT}/${name}.png`, Buffer.from(r.result.data, "base64"));
    },
    async click(sel: string) {
      const at = await center(sel);
      if (!at) throw new Error(`no element ${sel}`);
      await mouse("mouseMoved", at[0], at[1]);
      await mouse("mousePressed", at[0], at[1]);
      await mouse("mouseReleased", at[0], at[1]);
    },
    mouse,
    async type(text: string) {
      await send("Input.insertText", { text });
    },
    async phase() {
      return String(await evaluate(`document.querySelector("#console")?.dataset.phase`));
    },
    async waitPhase(want: string, timeout = 60_000) {
      const t = Date.now();
      while (Date.now() - t < timeout) {
        if ((await s.phase()) === want) return true;
        await sleep(100);
      }
      return false;
    },
    async close() {
      await fetch(`http://127.0.0.1:${PORT}/json/close/${target.id}`);
      ws.close();
    },
  };
  return s;
}
