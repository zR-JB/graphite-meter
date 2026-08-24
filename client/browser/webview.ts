import {
  afterAll,
  describe,
  expect as bunExpect,
  test as bunTest,
} from "bun:test";
import { mkdir } from "node:fs/promises";
import { resolve, sep } from "node:path";
import { createChromeWebView } from "./chrome";

type Name = string | RegExp;
type Step =
  | { kind: "css"; value: string }
  | { kind: "role"; role: string; name?: Name; exact?: boolean }
  | { kind: "text"; text: Name; exact?: boolean }
  | { kind: "label"; text: Name }
  | { kind: "filter"; text?: Name; has?: Step[] }
  | { kind: "nth"; index: number };

const artifacts = resolve(
  process.env.GM_WEBVIEW_ARTIFACTS ?? "test-results/webview",
);
const dist = resolve(process.env.GM_WEBVIEW_ROOT ?? "dist");
let server: ReturnType<typeof Bun.serve> | undefined;
const localRoutes: Array<{
  owner: Page;
  pattern: string;
  handler: RouteHandler;
}> = [];

function staticServer() {
  if (server) return server;
  server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      const url = new URL(request.url);
      const routeMatch = localRoutes.find(({ pattern }) =>
        glob(pattern, url.href),
      );
      if (process.env.GM_WEBVIEW_DEBUG)
        console.error(
          "webview request",
          url.href,
          routeMatch?.pattern ?? "static",
        );
      if (routeMatch) {
        const route = new LocalRoute(url.href);
        await routeMatch.handler(route);
        if (route.response) return route.response;
      }
      let relative = decodeURIComponent(url.pathname).replace(/^\/+/, "");
      if (!relative) relative = "index.html";
      let path = resolve(dist, relative);
      if (path !== dist && !path.startsWith(dist + sep))
        return new Response("not found", { status: 404 });
      let file = Bun.file(path);
      if (!(await file.exists()) && !relative.includes(".")) {
        path = resolve(dist, "index.html");
        file = Bun.file(path);
      }
      if (!(await file.exists()))
        return new Response("not found", { status: 404 });
      return new Response(file, { headers: { "cache-control": "no-store" } });
    },
  });
  return server;
}

const unsafeScriptCharMap: Record<string, string> = {
  "<": "\\u003C",
  ">": "\\u003E",
  "/": "\\u002F",
  "\u2028": "\\u2028",
  "\u2029": "\\u2029",
};

const serializedJSON = (value: unknown) =>
  JSON.stringify(value, (_key, item) =>
    item instanceof RegExp
      ? { __regexp: true, source: item.source, flags: item.flags }
      : item,
  );

const encode = (value: unknown) => {
  const json = serializedJSON(value);
  if (json === undefined) return "undefined";
  return json.replace(
    /[<>/\u2028\u2029]/g,
    (char) => unsafeScriptCharMap[char],
  );
};

const pageValue = (value: unknown) => {
  const json = serializedJSON(value);
  return json === undefined ? undefined : JSON.parse(json);
};

const resolver = String.raw`
const revive = value => value && value.__regexp ? new RegExp(value.source, value.flags) : value;
const match = (actual, wanted, exact = false) => {
  wanted = revive(wanted); actual = String(actual ?? "").replace(/\s+/g, " ").trim();
  return wanted instanceof RegExp ? wanted.test(actual) : exact ? actual === wanted : actual.includes(wanted);
};
const name = el => {
  const labelled = el.getAttribute("aria-labelledby");
  if (labelled) return labelled.split(/\s+/).map(id => document.getElementById(id)?.textContent ?? "").join(" ");
  if (el.getAttribute("aria-label")) return el.getAttribute("aria-label");
  if (el.labels?.length) return [...el.labels].map(label => label.textContent ?? "").join(" ");
  return el.getAttribute("alt") || el.getAttribute("title") || el.textContent || "";
};
const role = el => el.getAttribute("role") || (/^H[1-6]$/.test(el.tagName) ? "heading" : "") || ({BUTTON:"button",A:"link",DIALOG:"dialog",SUMMARY:"button",SELECT:"combobox",TEXTAREA:"textbox"}[el.tagName]) || (el.tagName === "INPUT" ? ({checkbox:"checkbox",radio:"radio",range:"slider",button:"button",submit:"button"}[el.type] || "textbox") : "");
const resolveSteps = steps => {
  let nodes = [document];
  for (const step of steps) {
    if (step.kind === "css") nodes = nodes.flatMap(root => [...root.querySelectorAll(step.value)]);
    else if (step.kind === "role") nodes = nodes.flatMap(root => [...root.querySelectorAll("*")].filter(el => role(el) === step.role && (step.name === undefined || match(name(el), step.name, step.exact))));
    else if (step.kind === "text") nodes = nodes.flatMap(root => [...root.querySelectorAll("*")].filter(el => match(el.textContent, step.text, step.exact) && ![...el.children].some(child => match(child.textContent, step.text, step.exact))));
    else if (step.kind === "label") nodes = nodes.flatMap(root => [...root.querySelectorAll("input,select,textarea,button")].filter(el => match(name(el), step.text)));
    else if (step.kind === "filter") nodes = nodes.filter(el => (step.text === undefined || match(el.textContent, step.text)) && (!step.has || resolveWithin(el, step.has).length));
    else if (step.kind === "nth") nodes = nodes.length ? [nodes.at(step.index)] : [];
  }
  return nodes.filter(Boolean);
};
const resolveWithin = (root, steps) => {
  const old = document.__gmRoot; document.__gmRoot = root;
  let nodes = [root];
  for (const step of steps) {
    if (step.kind === "css") nodes = nodes.flatMap(scope => [...scope.querySelectorAll(step.value)]);
    else if (step.kind === "role") nodes = nodes.flatMap(scope => [...scope.querySelectorAll("*")].filter(el => role(el) === step.role && (step.name === undefined || match(name(el), step.name, step.exact))));
    else if (step.kind === "text") nodes = nodes.flatMap(scope => [...scope.querySelectorAll("*")].filter(el => match(el.textContent, step.text, step.exact)));
    else if (step.kind === "label") nodes = nodes.flatMap(scope => [...scope.querySelectorAll("input,select,textarea,button")].filter(el => match(name(el), step.text)));
    else if (step.kind === "filter") nodes = nodes.filter(el => (step.text === undefined || match(el.textContent, step.text)) && (!step.has || resolveWithin(el, step.has).length));
    else if (step.kind === "nth") nodes = nodes.length ? [nodes.at(step.index)] : [];
  }
  document.__gmRoot = old; return nodes.filter(Boolean);
};`;

export class Locator {
  constructor(
    readonly page: Page,
    readonly steps: Step[],
  ) {}
  locator(value: string, options: { hasText?: Name; has?: Locator } = {}) {
    let result = new Locator(this.page, [
      ...this.steps,
      { kind: "css", value },
    ]);
    if (options.hasText !== undefined || options.has)
      result = result.filter({ hasText: options.hasText, has: options.has });
    return result;
  }
  getByRole(role: string, options: { name?: Name; exact?: boolean } = {}) {
    return new Locator(this.page, [
      ...this.steps,
      { kind: "role", role, name: options.name, exact: options.exact },
    ]);
  }
  getByText(text: Name, options: { exact?: boolean } = {}) {
    return new Locator(this.page, [
      ...this.steps,
      { kind: "text", text, exact: options.exact },
    ]);
  }
  getByLabel(text: Name) {
    return new Locator(this.page, [...this.steps, { kind: "label", text }]);
  }
  filter(options: { hasText?: Name; has?: Locator }) {
    return new Locator(this.page, [
      ...this.steps,
      { kind: "filter", text: options.hasText, has: options.has?.steps },
    ]);
  }
  first() {
    return new Locator(this.page, [...this.steps, { kind: "nth", index: 0 }]);
  }
  nth(index: number) {
    return new Locator(this.page, [...this.steps, { kind: "nth", index }]);
  }
  async evaluate<T>(
    fn: (element: any, arg?: any) => T,
    arg?: unknown,
  ): Promise<T> {
    return this.page.callFunction(
      `function(steps, arg) { ${resolver}; const elements = resolveSteps(steps); if (!elements[0]) throw new Error("locator found no element: " + JSON.stringify(steps)); return (${fn.toString()})(elements[0], arg); }`,
      [this.steps, arg],
    );
  }
  async evaluateAll<T>(
    fn: (elements: any[], arg?: any) => T,
    arg?: unknown,
  ): Promise<T> {
    return this.page.callFunction(
      `function(steps, arg) { ${resolver}; const elements = resolveSteps(steps); return (${fn.toString()})(elements, arg); }`,
      [this.steps, arg],
    );
  }
  async state() {
    return this.page.callFunction<any[]>(
      `function(steps) { ${resolver}; const elements = resolveSteps(steps); return elements.map(el => { const box=el.getBoundingClientRect(); const style=getComputedStyle(el); return { text:el.textContent ?? "", value:el.value, checked:!!el.checked, disabled:!!el.disabled, focused:document.activeElement===el, visible:!!(box.width||box.height) && style.visibility!=="hidden" && style.display!=="none", attrs:Object.fromEntries([...el.attributes].map(a=>[a.name,a.value])), box:{x:box.x,y:box.y,width:box.width,height:box.height} }; }); }`,
      [this.steps],
    );
  }
  async click() {
    await this.actionPoint(true);
    await this.evaluate((element) => element.click());
  }
  async hover() {
    const point = await this.actionPoint(false);
    await this.page.raw.cdp("Input.dispatchMouseEvent", {
      type: "mouseMoved",
      ...point,
    });
  }
  private actionPoint(requireEnabled: boolean) {
    return retry(async () => {
      const point = await this.evaluate<{
        x: number;
        y: number;
        actionable: boolean;
      }>((element, enabled) => {
        element.scrollIntoView({ block: "center", inline: "center" });
        const box = element.getBoundingClientRect();
        const x = box.x + box.width / 2;
        const y = box.y + box.height / 2;
        const hit = document.elementFromPoint(x, y);
        return {
          x,
          y,
          actionable:
            box.width > 0 &&
            box.height > 0 &&
            (!enabled || !element.disabled) &&
            !!hit &&
            (hit === element || element.contains(hit)),
        };
      }, requireEnabled);
      if (!point.actionable) throw new Error("locator is not actionable");
      return { x: point.x, y: point.y };
    });
  }
  async dispatchEvent(type: string, init: Record<string, unknown> = {}) {
    await this.evaluate(
      (element, event) => {
        const EventType = event.type.startsWith("pointer")
          ? PointerEvent
          : Event;
        element.dispatchEvent(
          new EventType(event.type, { ...event.init, bubbles: true }),
        );
      },
      { type, init },
    );
  }
  async focus() {
    await this.evaluate((el) => el.focus());
  }
  async press(key: string) {
    await this.focus();
    await pressKey(this.page.raw, key);
  }
  async fill(value: string) {
    await this.evaluate((element, text) => {
      element.focus();
      element.value = text;
      element.dispatchEvent(
        new InputEvent("input", {
          bubbles: true,
          inputType: "insertText",
          data: text,
        }),
      );
      element.dispatchEvent(new Event("change", { bubbles: true }));
    }, value);
  }
  async type(value: string) {
    await this.click();
    await this.page.raw.type(value);
  }
  async selectOption(value: string) {
    await this.evaluate((el, selected) => {
      el.value = selected;
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
    }, value);
  }
  async textContent() {
    return (await this.state())[0]?.text ?? null;
  }
  async innerText() {
    return this.evaluate<string>((element) => element.innerText);
  }
  async getAttribute(name: string) {
    return (await this.state())[0]?.attrs[name] ?? null;
  }
  async boundingBox() {
    return (await this.state())[0]?.box ?? null;
  }
  async scrollIntoViewIfNeeded() {
    await this.evaluate((element) =>
      element.scrollIntoView({ block: "center", inline: "center" }),
    );
  }
}

type RouteHandler = (route: LocalRoute) => void | Promise<void>;
class LocalRoute {
  response: Response | undefined;
  constructor(readonly url: string) {}
  request() {
    return { url: () => this.url };
  }
  async fulfill(options: {
    json?: unknown;
    body?: string;
    status?: number;
    headers?: Record<string, string>;
  }) {
    const body =
      options.json === undefined
        ? (options.body ?? "")
        : JSON.stringify(options.json);
    this.response = new Response(body, {
      status: options.status ?? 200,
      headers: {
        ...(options.json === undefined
          ? {}
          : { "content-type": "application/json" }),
        ...(options.headers ?? {}),
      },
    });
  }
  async abort() {
    this.response = new Response("aborted", { status: 503 });
  }
  async continue() {}
}
export class Page {
  readonly raw: Bun.WebView;
  readonly errors: string[] = [];
  readonly console: string[] = [];
  private blockedPatterns: string[] = [];
  private initialized = false;
  private firstNavigation = true;
  private storageCleanupScript: { identifier: string } | undefined;
  private pageErrorHandlers: Array<(error: Error) => void> = [];
  constructor() {
    this.raw = createChromeWebView((type, ...args) =>
      this.console.push(`${type}: ${args.map(String).join(" ")}`),
    );
  }
  locator(value: string, options: { hasText?: Name; has?: Locator } = {}) {
    return new Locator(this, [{ kind: "css", value }]).filter(options);
  }
  getByRole(role: string, options: { name?: Name; exact?: boolean } = {}) {
    return new Locator(this, [
      { kind: "role", role, name: options.name, exact: options.exact },
    ]);
  }
  getByText(text: Name, options: { exact?: boolean } = {}) {
    return new Locator(this, [{ kind: "text", text, exact: options.exact }]);
  }
  getByLabel(text: Name) {
    return new Locator(this, [{ kind: "label", text }]);
  }
  private async init() {
    if (this.initialized) return;
    await this.raw.navigate("about:blank");
    await this.raw.cdp("Runtime.enable");
    this.storageCleanupScript = await this.raw.cdp(
      "Page.addScriptToEvaluateOnNewDocument",
      { source: "localStorage.clear(); sessionStorage.clear();" },
    );
    this.raw.addEventListener("Runtime.exceptionThrown", ((
      event: MessageEvent<any>,
    ) => {
      const message =
        event.data.exceptionDetails?.exception?.description ??
        event.data.exceptionDetails?.text ??
        "page exception";
      this.errors.push(message);
      for (const handler of this.pageErrorHandlers) handler(new Error(message));
    }) as EventListener);
    this.initialized = true;
  }
  async callFunction<T>(
    functionDeclaration: string,
    args: unknown[],
  ): Promise<T> {
    await this.init();
    const global = await this.raw.cdp<{
      result?: { objectId?: string };
    }>("Runtime.evaluate", { expression: "globalThis" });
    const objectId = global.result?.objectId;
    if (!objectId) throw new Error("CDP did not return the page global object");
    try {
      const outcome = await this.raw.cdp<{
        result?: { value?: T; description?: string };
        exceptionDetails?: {
          text?: string;
          exception?: { description?: string };
        };
      }>("Runtime.callFunctionOn", {
        objectId,
        functionDeclaration,
        arguments: args.map((value) =>
          value === undefined ? {} : { value: pageValue(value) },
        ),
        awaitPromise: true,
        returnByValue: true,
      });
      if (outcome.exceptionDetails) {
        throw new Error(
          outcome.exceptionDetails.exception?.description ??
            outcome.exceptionDetails.text ??
            "page function failed",
        );
      }
      return outcome.result?.value as T;
    } finally {
      await this.raw.cdp("Runtime.releaseObject", { objectId }).catch(() => {});
    }
  }
  async route(pattern: string, handler: RouteHandler) {
    if (pattern.startsWith("**")) {
      localRoutes.push({ owner: this, pattern, handler });
    } else {
      // Cross-origin routes in these tests only simulate an unreachable
      // endpoint. CDP's URL blocker avoids Fetch.requestPaused re-entrancy
      // while WebView is synchronously waiting on navigation or a click.
      void handler;
      this.blockedPatterns.push(pattern);
      await this.init();
      await this.raw.cdp("Network.enable");
      await this.raw.cdp("Network.setBlockedURLs", {
        urls: this.blockedPatterns,
      });
    }
  }
  async addInitScript(fn: (...args: any[]) => unknown, arg?: unknown) {
    await this.init();
    await this.raw.cdp("Page.addScriptToEvaluateOnNewDocument", {
      source: `(${fn.toString()})(${encode(arg)})`,
    });
  }
  async goto(url: string) {
    await this.init();
    const base = `http://${staticServer().hostname}:${staticServer().port}`;
    await this.raw.navigate(new URL(url, base).href);
    if (this.firstNavigation && this.storageCleanupScript)
      await this.raw.cdp(
        "Page.removeScriptToEvaluateOnNewDocument",
        this.storageCleanupScript,
      );
    this.firstNavigation = false;
  }
  async reload() {
    await this.raw.reload();
  }
  async evaluate<T>(
    fn: ((arg?: any) => T) | string,
    arg?: unknown,
  ): Promise<T> {
    return this.raw.evaluate(
      typeof fn === "string" ? fn : `(${fn.toString()})(${encode(arg)})`,
    );
  }
  async setViewportSize(size: { width: number; height: number }) {
    await this.init();
    await this.raw.resize(size.width, size.height);
  }
  async waitForTimeout(ms: number) {
    await Bun.sleep(ms);
  }
  async emulateMedia(options: { reducedMotion?: "reduce" | "no-preference" }) {
    await this.init();
    await this.raw.cdp("Emulation.setEmulatedMedia", {
      features: options.reducedMotion
        ? [{ name: "prefers-reduced-motion", value: options.reducedMotion }]
        : [],
    });
  }
  on(event: string, callback: (error: Error) => void) {
    if (event === "pageerror") this.pageErrorHandlers.push(callback);
  }
  readonly keyboard = { press: (key: string) => pressKey(this.raw, key) };
  readonly mouse = {
    move: async (x: number, y: number) => {
      await this.raw.cdp("Input.dispatchMouseEvent", {
        type: "mouseMoved",
        x,
        y,
      });
    },
  };
  readonly context = {
    newCDPSession: async (_page?: Page) => ({
      send: (method: string, params?: Record<string, unknown>) =>
        this.raw.cdp(method, params),
    }),
  };
  async artifact(name: string) {
    await mkdir(artifacts, { recursive: true });
    const stem = name.replace(/[^a-z0-9_.-]+/gi, "-").slice(0, 100);
    await Bun.write(
      resolve(artifacts, `${stem}.png`),
      await this.raw.screenshot(),
    );
    const dom = await this.raw
      .evaluate<string>("document.documentElement.outerHTML.slice(0, 50000)")
      .catch(() => "");
    await Bun.write(
      resolve(artifacts, `${stem}.txt`),
      `URL: ${this.raw.url}\n\nERRORS\n${this.errors.join("\n")}\n\nCONSOLE\n${this.console.join("\n")}\n\nDOM\n${dom}`,
    );
  }
  close() {
    for (let index = localRoutes.length - 1; index >= 0; index--)
      if (localRoutes[index].owner === this) localRoutes.splice(index, 1);
    this.raw.close();
  }
}

function glob(pattern: string, url: string) {
  const escaped = pattern
    .split("**")
    .map((part) =>
      part.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, "[^/]*"),
    )
    .join(".*");
  return new RegExp(`^${escaped}$`).test(url);
}

function pressKey(view: Bun.WebView, chord: string) {
  const parts = chord.split("+");
  const key = parts.pop()!;
  return view.press(key, {
    modifiers: parts as Bun.WebView.Modifier[],
  });
}

const retry = async <T>(
  check: () => Promise<T>,
  timeout = 5000,
): Promise<T> => {
  const end = Date.now() + timeout;
  let last: unknown;
  do {
    try {
      return await check();
    } catch (error) {
      last = error;
      await Bun.sleep(40);
    }
  } while (Date.now() < end);
  throw last;
};
function locatorExpect(locator: Locator, negative = false) {
  const assertion = (
    predicate: (state: any[]) => boolean,
    message: string,
    timeout?: number,
  ) =>
    retry(async () => {
      const state = await locator.state();
      if (predicate(state) === negative)
        throw new Error(
          `${negative ? "not " : ""}${message}: ${JSON.stringify(state)}`,
        );
    }, timeout);
  return {
    get not() {
      return locatorExpect(locator, !negative);
    },
    toHaveCount: (count: number, options?: { timeout?: number }) =>
      assertion(
        (s) => s.length === count,
        `to have count ${count}`,
        options?.timeout,
      ),
    toBeVisible: (options?: { timeout?: number }) =>
      assertion((s) => !!s[0]?.visible, "to be visible", options?.timeout),
    toBeHidden: (options?: { timeout?: number }) =>
      assertion((s) => !s[0]?.visible, "to be hidden", options?.timeout),
    toHaveAttribute: (name: string, value: string | RegExp) =>
      assertion(
        (s) => matchValue(s[0]?.attrs[name], value, true),
        `to have attribute ${name}`,
      ),
    toHaveText: (value: string | RegExp) =>
      assertion(
        (s) => matchValue(s.map((x) => x.text).join(" "), value, true),
        "to have text",
      ),
    toContainText: (value: string | RegExp) =>
      assertion(
        (s) => matchValue(s.map((x) => x.text).join(" "), value, false),
        "to contain text",
      ),
    toHaveClass: (value: string | RegExp) =>
      assertion(
        (s) => matchValue(s[0]?.attrs.class, value, false),
        "to have class",
      ),
    toBeChecked: () => assertion((s) => !!s[0]?.checked, "to be checked"),
    toBeEnabled: () =>
      assertion((s) => !!s[0] && !s[0].disabled, "to be enabled"),
    toBeDisabled: () => assertion((s) => !!s[0]?.disabled, "to be disabled"),
    toBeFocused: () => assertion((s) => !!s[0]?.focused, "to be focused"),
    toHaveValue: (value: string) =>
      assertion((s) => s[0]?.value === value, `to have value ${value}`),
    toHaveCSS: (property: string, value: string | RegExp) =>
      retry(async () => {
        const actual = await locator.evaluate(
          (element, name) => getComputedStyle(element).getPropertyValue(name),
          property,
        );
        if (matchValue(actual, value, true) === negative)
          throw new Error(
            `${negative ? "not " : ""}to have CSS ${property}: ${actual}`,
          );
      }),
  };
}
function matchValue(actual: unknown, wanted: string | RegExp, exact: boolean) {
  const text = String(actual ?? "")
    .replace(/\s+/g, " ")
    .trim();
  return wanted instanceof RegExp
    ? wanted.test(text)
    : exact
      ? text === wanted
      : text.includes(wanted);
}

function pollExpect(
  fn: () => unknown | Promise<unknown>,
  options?: { timeout?: number },
) {
  const make = (negative = false): any =>
    new Proxy(
      {},
      {
        get(_target, property) {
          if (property === "not") return make(!negative);
          return (...args: unknown[]) =>
            retry(async () => {
              const assertion: any = bunExpect(await fn());
              const matchers: any = negative ? assertion.not : assertion;
              matchers[property](...args);
            }, options?.timeout);
        },
      },
    );
  return make();
}
export const expect: any = Object.assign(
  (actual: unknown) =>
    actual instanceof Locator ? locatorExpect(actual) : bunExpect(actual),
  { poll: pollExpect },
);
type Fixtures = {
  page: Page;
  browserName: "chromium";
  context: Page["context"];
};
class SkipTest extends Error {}
interface WebViewTest {
  (name: string, fn: (fixtures: Fixtures) => unknown): void;
  describe: typeof describe;
  afterAll: typeof afterAll;
  skip(condition?: unknown, reason?: string): void;
  info(): { project: { name: "chromium" } };
}
export const test: WebViewTest = Object.assign(
  (name: string, fn: (fixtures: Fixtures) => unknown) =>
    bunTest(
      name,
      async () => {
        const page = new Page();
        const fixtures = {
          page,
          browserName: "chromium" as const,
          context: page.context,
        };
        try {
          await fn(fixtures);
          if (page.errors.length) throw new Error(page.errors.join("\n"));
        } catch (error) {
          if (error instanceof SkipTest) return;
          await page.artifact(name).catch(() => {});
          throw error;
        } finally {
          page.close();
        }
      },
      { retry: process.env.CI ? 1 : 0 },
    ),
  {
    describe,
    afterAll,
    skip: (condition?: unknown, reason?: string) => {
      if (condition) throw new SkipTest(reason ?? "skipped");
    },
    info: () => ({ project: { name: "chromium" as const } }),
  },
);

export class AxeBuilder {
  private selector: string | undefined;
  constructor(private options: { page: Page }) {}
  include(selector: string) {
    this.selector = selector;
    return this;
  }
  async analyze() {
    const source = await Bun.file(
      resolve("node_modules/axe-core/axe.min.js"),
    ).text();
    await this.options.page.raw.evaluate(
      `(() => { ${source}; return true })()`,
    );
    return this.options.page.raw.evaluate<any>(
      `axe.run(${this.selector ? encode(this.selector) : "document"})`,
    );
  }
}

// Every Page closes in its test's finally block. This synchronous exit hook is
// the final shared-process guard, which WebView itself also performs at exit.
process.on("exit", () => {
  server?.stop(true);
  Bun.WebView.closeAll();
});
