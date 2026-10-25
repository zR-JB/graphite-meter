// Chrome for Testing through Bun's WebView (CDP) or Firefox through WebDriver BiDi, by GM_E2E_BROWSER.
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
        const dir = /--user-data-dir=([^\0\s]+\.bun-chrome)(?:[\0\s]|$)/.exec(cmdline);
        if (dir?.[1]) profiles.push(dir[1]);
      }
  } catch {}
  Bun.WebView.closeAll();
  for (const dir of profiles)
    try {
      if (dirname(dir) === tmpdir()) rmSync(dir, { recursive: true, force: true, maxRetries: 5 });
    } catch {}
}
// Chrome is shared across files, so an afterAll would kill the next file's first view; shut down only at exit.
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
        args.map((arg) => (arg as { description?: string })?.description ?? arg).join(" "),
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
    hover: (x, y) => view.cdp("Input.dispatchMouseEvent", { type: "mouseMoved", x, y }),
    press: (key) => view.press(key),
    touch: (phase, x, y) =>
      view.cdp("Input.dispatchTouchEvent", {
        type: `touch${phase[0]!.toUpperCase()}${phase.slice(1)}`,
        touchPoints: phase === "end" ? [] : [{ x, y }],
      }),
    resize: (width, height) => view.resize(width, height),
    addInitScript: (source) => view.cdp("Page.addScriptToEvaluateOnNewDocument", { source }),
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
      Buffer.from((await view.cdp<{ data: string }>("Page.captureScreenshot")).data, "base64"),
    async close() {
      // A page with a test still running asks before it is left; leaving answers yes.
      await view.cdp("Page.enable");
      view.addEventListener("Page.javascriptDialogOpening", () =>
        view.cdp("Page.handleJavaScriptDialog", { accept: true }).catch(() => {}),
      );
      await view.navigate("about:blank");
      view.close();
    },
    cdp: (method, params) => view.cdp(method, params),
    onCdp: (event, listener) => view.addEventListener(event, listener),
  };
}

// WebDriver's names for the keys the suite presses.
const KEYS: Record<string, string> = {
  Escape: "\uE00C",
  Tab: "\uE004",
  Enter: "\uE007",
  Backspace: "\uE003",
  ArrowLeft: "\uE012",
  ArrowUp: "\uE013",
  ArrowRight: "\uE014",
  ArrowDown: "\uE015",
  Home: "\uE011",
  End: "\uE010",
};

// Firefox logs refused requests as errors and Chrome reports nothing; the app handles both failures itself.
const NETWORK_REPORT =
  /^(Cross-Origin Request Blocked|Firefox can.t establish a connection|The connection to .* was interrupted)/;

interface Bidi {
  send<T = any>(method: string, params?: object): Promise<T>;
  on(listener: (method: string, params: any) => void): () => void;
}

// One Firefox per test process; each page is a user context of its own, with its own storage and cookies.
let firefox: { bidi: Promise<Bidi>; stop(): Promise<void> } | undefined;

/** Ends this process's browser, so the next page gets a new one without the old one's network state. */
export async function freshBrowser() {
  if (browser === "chrome") return removeProfiles();
  await firefox?.stop();
}
function launchFirefox(): Promise<Bidi> {
  if (firefox) return firefox.bidi;
  const profile = mkdtempSync(join(tmpdir(), "gm-firefox-"));
  let child: Bun.Subprocess<"ignore", "ignore", "pipe"> | undefined;
  const remove = () => {
    child?.kill();
    try {
      rmSync(profile, { recursive: true, force: true });
    } catch {}
  };
  process.on("exit", remove);
  async function stop(): Promise<void> {
    if (firefox === launch) firefox = undefined;
    process.off("exit", remove);
    if (child) {
      child.kill();
      await Promise.race([child.exited, Bun.sleep(5_000)]);
      child.kill("SIGKILL");
      await child.exited;
    }
    remove();
  }
  let banner = "";
  const late = new Promise<never>((_, fail) =>
    setTimeout(
      () => fail(new Error(`Firefox started no WebDriver BiDi session in 30 s: ${banner}`)),
      30_000,
    ).unref(),
  );
  const launch = { bidi: Promise.race([start(), late]), stop };
  firefox = launch;
  launch.bidi.catch(stop);
  return launch.bidi;

  async function start(): Promise<Bidi> {
    const ca = process.env.GM_E2E_CA;
    // JSON loads as a plain document of its origin, as in Chrome, so a test can work on that origin from it.
    writeFileSync(
      join(profile, "user.js"),
      [
        'user_pref("devtools.jsonview.enabled", false);',
        // A runner has no pointing device, so Firefox would report a phone's pointer; the suite tests a desktop mouse, as Chrome reports.
        'user_pref("ui.primaryPointerCapabilities", 6);',
        'user_pref("ui.allPointerCapabilities", 6);',
        // Machine policies can install extensions that hold requests mid-test; a proxy nobody listens on keeps them out, and loopback is never proxied.
        'user_pref("network.proxy.type", 1);',
        'user_pref("network.proxy.http", "127.0.0.1");',
        'user_pref("network.proxy.http_port", 9);',
        'user_pref("network.proxy.ssl", "127.0.0.1");',
        'user_pref("network.proxy.ssl_port", 9);',
        ...(ca
          ? ['user_pref("network.http.http3.disable_when_third_party_roots_found", false);']
          : []),
        "",
      ].join("\n"),
    );
    // Firefox speaks QUIC only to a root it trusts, so the network group's profile trusts the run's CA.
    if (ca)
      for (const args of [
        ["-N", "--empty-password"],
        ["-A", "-n", "graphite-meter-e2e", "-t", "C,,", "-i", ca],
      ]) {
        const certutil = Bun.spawnSync(["certutil", "-d", `sql:${profile}`, ...args]);
        if (certutil.exitCode !== 0) throw new Error(`certutil ${args[0]}: ${certutil.stderr}`);
      }
    child = Bun.spawn(
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
    const reader = child.stderr.getReader();
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
    let closed: Error | undefined;
    socket.onclose = () => {
      closed = new Error("Firefox closed its WebDriver BiDi session");
      for (const call of pending.values()) call.reject(closed);
      pending.clear();
      void stop();
    };
    socket.onmessage = ({ data }) => {
      const message = JSON.parse(String(data));
      const call = pending.get(message.id);
      if (call) {
        pending.delete(message.id);
        if (message.type === "error")
          call.reject(new Error(`${message.error}: ${message.message}`));
        else call.resolve(message.result);
      } else if (message.method)
        for (const listener of listeners) listener(message.method, message.params);
    };
    const bidi: Bidi = {
      send(method, params = {}) {
        if (closed) return Promise.reject(closed);
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
    await bidi.send("session.subscribe", { events: ["log.entryAdded"] });
    return bidi;
  }
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
    await intercept(blocked.length > 0);
    if (previous) await bidi.send("browser.removeUserContext", { userContext: previous });
  }
  let blocked: RegExp[] = [];
  // Network events tee every response through Firefox's network observer, which can wedge HTTP/2 downloads.
  let interception: { intercept: string; subscription: string } | undefined;
  async function intercept(on: boolean) {
    if (interception) {
      await bidi.send("network.removeIntercept", {
        intercept: interception.intercept,
      });
      await bidi.send("session.unsubscribe", {
        subscriptions: [interception.subscription],
      });
      interception = undefined;
    }
    if (on)
      interception = {
        subscription: (
          await bidi.send("session.subscribe", {
            events: ["network.beforeRequestSent"],
            contexts: [context],
          })
        ).subscription,
        intercept: (
          await bidi.send("network.addIntercept", {
            phases: ["beforeRequestSent"],
            contexts: [context],
          })
        ).intercept,
      };
  }
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
              (frame: { functionName: string; url: string; lineNumber: number }) =>
                `    at ${frame.functionName || "<anonymous>"} (${frame.url}:${frame.lineNumber + 1})`,
            ),
          ].join("\n"),
        );
      else events.console(params.method ?? params.level, params.text);
    }
    if (method === "network.beforeRequestSent" && params.context === context && params.isBlocked) {
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
        result.exceptionDetails.exception?.value?.message ?? result.exceptionDetails.text,
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
      await pointer([{ type: "pointerMove", x: Math.round(x), y: Math.round(y) }]);
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
              .map((part) => RegExp.escape(part))
              .join(".*")}$`,
          ),
      );
      if (!blocked.length || !interception) await intercept(blocked.length > 0);
    },
    screenshot: async () =>
      Buffer.from(
        (await bidi.send("browsingContext.captureScreenshot", { context })).data,
        "base64",
      ),
    async close() {
      off();
      await intercept(false);
      await bidi.send("browser.removeUserContext", { userContext });
    },
  };
}
