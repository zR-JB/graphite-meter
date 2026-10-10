// The browsers the suite drives: Chrome for Testing through Bun's WebView (CDP), and Firefox through WebDriver
// BiDi. GM_E2E_BROWSER picks one; a page sees only the Driver both share.
import {
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

export const browser: "chrome" | "firefox" =
  process.env.GM_E2E_BROWSER === "firefox" ? "firefox" : "chrome";

export interface Driver {
  /** Evaluates an expression, awaiting a promise; the value comes back as JSON. */
  evaluate<T>(source: string): Promise<T>;
  navigate(url: string): Promise<void>;
  reload(): Promise<void>;
  url(): Promise<string>;
  click(x: number, y: number): Promise<void>;
  hover(x: number, y: number): Promise<void>;
  press(key: string): Promise<void>;
  /** One finger: down at a point, moved, then lifted. */
  touch(phase: "start" | "move" | "end", x: number, y: number): Promise<void>;
  resize(width: number, height: number): Promise<void>;
  /** Runs before any page script in every document the view loads. */
  addInitScript(source: string): Promise<void>;
  /** Forgets cookies and the origins' local storage and IndexedDB, as a new visitor. */
  clearStorage(origins: string[]): Promise<void>;
  /** Fails requests whose URL matches a `*` pattern; an empty list stops blocking. */
  blockRequests(patterns: string[]): Promise<void>;
  screenshot(): Promise<Uint8Array>;
  close(): Promise<void>;
  /** Chrome DevTools Protocol; Firefox has none. */
  cdp?<T = any>(method: string, params?: Record<string, unknown>): Promise<T>;
  onCdp?(event: string, listener: (event: unknown) => void): void;
}

export interface DriverEvents {
  console(type: string, line: string): void;
  exception(text: string): void;
}

const VIEWPORT = { width: 1280, height: 800 };

export async function openDriver(events: DriverEvents): Promise<Driver> {
  return browser === "firefox" ? firefoxDriver(events) : chromeDriver(events);
}

// Bun never removes the temp profile of the Chrome it spawns; this process's live Chrome names it (Linux /proc).
function removeProfiles() {
  const tasks = `/proc/${process.pid}/task`;
  const profiles: string[] = [];
  try {
    for (const task of readdirSync(tasks))
      for (const pid of readFileSync(`${tasks}/${task}/children`, "utf8")
        .split(" ")
        .filter(Boolean)) {
        const cmdline = readFileSync(`/proc/${pid}/cmdline`, "utf8");
        // Chrome rewrites its argv into one space-separated title.
        const dir = /--user-data-dir=([^\0\s]+\.bun-chrome)(?:[\0\s]|$)/.exec(
          cmdline,
        );
        if (dir) profiles.push(dir[1]);
      }
  } catch {}
  Bun.WebView.closeAll();
  for (const dir of profiles)
    try {
      if (dirname(dir) === tmpdir())
        rmSync(dir, { recursive: true, force: true, maxRetries: 5 });
    } catch {}
}
// Chrome is shared across files in this process. File-scoped afterAll cleanup
// can kill the next file's first view while it starts. Only shut down at exit;
// the fleet wrapper also owns worker profiles through its temporary directory.
if (browser === "chrome") process.on("exit", removeProfiles);

async function chromeDriver(events: DriverEvents): Promise<Driver> {
  const view = new Bun.WebView({
    ...VIEWPORT,
    backend: {
      type: "chrome",
      url: false,
      path: process.env.BUN_CHROME_PATH,
      argv: [
        "--hide-scrollbars",
        ...(process.env.BUN_CHROME_ARGS ?? "").split(/\s+/).filter(Boolean),
      ],
      stderr: process.env.GM_WEBVIEW_DEBUG ? "inherit" : "ignore",
    },
    dataStore: "ephemeral",
    console: (type, ...args) =>
      events.console(
        type,
        args
          .map((arg) => (arg as { description?: string })?.description ?? arg)
          .join(" "),
      ),
  });
  await view.navigate("about:blank");
  await view.cdp("Runtime.enable");
  view.addEventListener("Runtime.exceptionThrown", (event: any) => {
    const details = event.data.exceptionDetails;
    events.exception(details.exception?.description ?? details.text);
  });
  return {
    evaluate: (source) => view.evaluate(source),
    navigate: (url) => view.navigate(url),
    reload: () => view.reload(),
    url: async () => view.url,
    click: (x, y) => view.click(x, y),
    hover: (x, y) =>
      view.cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x, y }),
    press: (key) => view.press(key),
    touch: (phase, x, y) =>
      view.cdp("Input.dispatchTouchEvent", {
        type: `touch${phase[0]!.toUpperCase()}${phase.slice(1)}`,
        touchPoints: phase === "end" ? [] : [{ x, y }],
      }),
    resize: (width, height) => view.resize(width, height),
    addInitScript: (source) =>
      view.cdp("Page.addScriptToEvaluateOnNewDocument", { source }),
    async clearStorage(origins) {
      await view.cdp("Network.clearBrowserCookies");
      for (const origin of origins)
        await view.cdp("Storage.clearDataForOrigin", {
          origin,
          storageTypes: "local_storage,indexeddb",
        });
    },
    async blockRequests(urls) {
      await view.cdp("Network.enable");
      await view.cdp("Network.setBlockedURLs", { urls });
    },
    screenshot: async () =>
      Buffer.from(
        (await view.cdp<{ data: string }>("Page.captureScreenshot")).data,
        "base64",
      ),
    async close() {
      await view.navigate("about:blank");
      view.close();
    },
    cdp: (method, params) => view.cdp(method, params),
    onCdp: (event, listener) => view.addEventListener(event, listener),
  };
}

// WebDriver's names for the keys the suite presses.
const KEYS: Record<string, string> = {
  Escape: "",
  Tab: "",
  Enter: "",
  Backspace: "",
  ArrowLeft: "",
  ArrowUp: "",
  ArrowRight: "",
  ArrowDown: "",
  Home: "",
  End: "",
};

// Firefox logs a refused or failed request as an error; Chrome tells pages' console listeners nothing, and the app
// handles both failures itself.
const NETWORK_REPORT =
  /^(Cross-Origin Request Blocked|Firefox can.t establish a connection|The connection to .* was interrupted)/;

interface Bidi {
  send<T = any>(method: string, params?: object): Promise<T>;
  on(listener: (method: string, params: any) => void): () => void;
}

// One Firefox per test process; each page is a user context of its own, with its own storage and cookies.
let firefox: Promise<Bidi> | undefined;
function launchFirefox(): Promise<Bidi> {
  return (firefox ??= (async () => {
    const profile = mkdtempSync(join(tmpdir(), "gm-firefox-"));
    // JSON loads as a plain document of its origin, as in Chrome, so a test can work on that origin from it.
    writeFileSync(
      join(profile, "user.js"),
      'user_pref("devtools.jsonview.enabled", false);\n',
    );
    const child = Bun.spawn(
      [
        process.env.GM_FIREFOX_PATH ?? "firefox",
        "--headless",
        "--no-remote",
        "--profile",
        profile,
        "--remote-debugging-port=0",
      ],
      { stdout: "ignore", stderr: "pipe" },
    );
    process.on("exit", () => {
      child.kill();
      rmSync(profile, { recursive: true, force: true });
    });
    const reader = child.stderr.getReader();
    let banner = "";
    let endpoint: string | undefined;
    while (!endpoint) {
      const { value, done } = await reader.read();
      if (done) throw new Error(`Firefox exited before listening: ${banner}`);
      banner += new TextDecoder().decode(value);
      endpoint = /WebDriver BiDi listening on (ws:\/\/\S+)/.exec(banner)?.[1];
    }
    // Firefox stops when its stderr fills, so keep draining it.
    void (async () => {
      while (!(await reader.read()).done);
    })();
    const socket = new WebSocket(`${endpoint}/session`);
    await new Promise((open, fail) => {
      socket.onopen = open;
      socket.onerror = fail;
    });
    let id = 0;
    const pending = new Map<number, PromiseWithResolvers<any>>();
    const listeners = new Set<(method: string, params: any) => void>();
    socket.onmessage = ({ data }) => {
      const message = JSON.parse(String(data));
      const call = pending.get(message.id);
      if (call) {
        pending.delete(message.id);
        if (message.type === "error")
          call.reject(new Error(`${message.error}: ${message.message}`));
        else call.resolve(message.result);
      } else if (message.method)
        for (const listener of listeners)
          listener(message.method, message.params);
    };
    const bidi: Bidi = {
      send(method, params = {}) {
        const call = Promise.withResolvers<any>();
        pending.set(++id, call);
        socket.send(JSON.stringify({ id, method, params }));
        return call.promise;
      },
      on(listener) {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
    };
    // The suite's servers present a certificate made for the run.
    await bidi.send("session.new", {
      capabilities: { alwaysMatch: { acceptInsecureCerts: true } },
    });
    await bidi.send("session.subscribe", {
      events: ["log.entryAdded", "network.beforeRequestSent"],
    });
    return bidi;
  })());
}

async function firefoxDriver(events: DriverEvents): Promise<Driver> {
  const bidi = await launchFirefox();
  // A page is a user context of its own; clearing its storage moves it to a new one with the same scripts.
  let userContext = "";
  let context = "";
  let viewport = VIEWPORT;
  const scripts: string[] = [];
  const preload = (source: string) =>
    // Firefox also runs them in the opaque-origin documents a navigation passes through, where storage throws.
    bidi.send("script.addPreloadScript", {
      functionDeclaration: `() => { if (origin !== "null") { ${source} } }`,
      contexts: [context],
    });
  async function begin() {
    const previous = userContext;
    ({ userContext } = await bidi.send("browser.createUserContext"));
    ({ context } = await bidi.send("browsingContext.create", {
      type: "tab",
      userContext,
    }));
    await bidi.send("browsingContext.setViewport", { context, viewport });
    for (const source of scripts) await preload(source);
    intercept = blocked.length
      ? (
          await bidi.send("network.addIntercept", {
            phases: ["beforeRequestSent"],
            contexts: [context],
          })
        ).intercept
      : undefined;
    if (previous)
      await bidi.send("browser.removeUserContext", { userContext: previous });
  }
  let blocked: RegExp[] = [];
  let intercept: string | undefined;
  await begin();
  const off = bidi.on((method, params) => {
    if (
      method === "log.entryAdded" &&
      params.source?.context === context &&
      !NETWORK_REPORT.test(params.text)
    ) {
      if (params.type === "javascript")
        events.exception(
          [
            params.text,
            ...(params.stackTrace?.callFrames ?? []).map(
              (frame: {
                functionName: string;
                url: string;
                lineNumber: number;
              }) =>
                `    at ${frame.functionName || "<anonymous>"} (${frame.url}:${frame.lineNumber + 1})`,
            ),
          ].join("\n"),
        );
      else events.console(params.method ?? params.level, params.text);
    }
    if (
      method === "network.beforeRequestSent" &&
      params.context === context &&
      params.isBlocked
    ) {
      const { request } = params.request;
      void bidi
        .send(
          blocked.some((pattern) => pattern.test(params.request.url))
            ? "network.failRequest"
            : "network.continueRequest",
          { request },
        )
        .catch(() => {});
    }
  });
  const evaluate = async <T>(source: string): Promise<T> => {
    const result = await bidi.send("script.evaluate", {
      expression: `(async () => JSON.stringify(await (${source})))()`,
      target: { context },
      awaitPromise: true,
    });
    if (result.type === "exception")
      throw new Error(
        result.exceptionDetails.exception?.value?.message ??
          result.exceptionDetails.text,
      );
    const value = result.result.value;
    return value === undefined ? (undefined as T) : JSON.parse(value);
  };
  const pointer = (actions: object[], pointerType = "mouse") =>
    bidi.send("input.performActions", {
      context,
      actions: [
        {
          type: "pointer",
          id: pointerType,
          parameters: { pointerType },
          actions,
        },
      ],
    });
  return {
    evaluate,
    async navigate(url) {
      await bidi.send("browsingContext.navigate", {
        context,
        url,
        wait: "complete",
      });
    },
    async reload() {
      await bidi.send("browsingContext.reload", { context, wait: "complete" });
    },
    url: () => evaluate<string>("location.href"),
    async click(x, y) {
      await pointer([
        { type: "pointerMove", x: Math.round(x), y: Math.round(y) },
        { type: "pointerDown", button: 0 },
        { type: "pointerUp", button: 0 },
      ]);
    },
    async hover(x, y) {
      await pointer([
        { type: "pointerMove", x: Math.round(x), y: Math.round(y) },
      ]);
    },
    async press(key) {
      const value = KEYS[key] ?? key;
      await bidi.send("input.performActions", {
        context,
        actions: [
          {
            type: "key",
            id: "keyboard",
            actions: [
              { type: "keyDown", value },
              { type: "keyUp", value },
            ],
          },
        ],
      });
    },
    async touch(phase, x, y) {
      const at = { type: "pointerMove", x: Math.round(x), y: Math.round(y) };
      await pointer(
        phase === "start"
          ? [at, { type: "pointerDown", button: 0 }]
          : phase === "move"
            ? [at]
            : [{ type: "pointerUp", button: 0 }],
        "touch",
      );
    },
    async resize(width, height) {
      viewport = { width, height };
      await bidi.send("browsingContext.setViewport", { context, viewport });
    },
    async addInitScript(source) {
      scripts.push(source);
      await preload(source);
    },
    clearStorage: begin,
    async blockRequests(patterns) {
      blocked = patterns.map(
        (pattern) =>
          new RegExp(
            `^${pattern
              .split("*")
              .map((part) => part.replace(/[\\^$.+?()[\]{}|/-]/g, "\\$&"))
              .join(".*")}$`,
          ),
      );
      if (blocked.length && !intercept)
        ({ intercept } = await bidi.send("network.addIntercept", {
          phases: ["beforeRequestSent"],
          contexts: [context],
        }));
      else if (!blocked.length && intercept) {
        await bidi.send("network.removeIntercept", { intercept });
        intercept = undefined;
      }
    },
    screenshot: async () =>
      Buffer.from(
        (await bidi.send("browsingContext.captureScreenshot", { context }))
          .data,
        "base64",
      ),
    async close() {
      off();
      await bidi.send("browser.removeUserContext", { userContext });
    },
  };
}
