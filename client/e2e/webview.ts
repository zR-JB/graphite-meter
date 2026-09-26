import { expect as bunExpect, test as bunTest } from "bun:test";
import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";

type Name = string | RegExp;
type Step =
  | { kind: "css"; value: string; text?: Name }
  | { kind: "role"; role: string; name?: Name; exact?: boolean }
  | { kind: "nth"; index: number };
interface ElementState {
  text: string;
  value?: string;
  disabled: boolean;
  focused: boolean;
  visible: boolean;
  attrs: Record<string, string>;
}

const artifacts = resolve(
  process.env.GM_WEBVIEW_ARTIFACTS ?? "test-results/webview",
);
// JSON is already a valid JavaScript expression for CDP evaluation.
const encode = (value: unknown) =>
  JSON.stringify(value, (_key, item) =>
    item instanceof RegExp ? { source: item.source, flags: item.flags } : item,
  ) ?? "undefined";

// Serialized into the page; names follow accname closely enough to skip aria-hidden text.
function resolveSteps(steps: Step[]): Element[] {
  const norm = (value: unknown) =>
    String(value ?? "")
      .replace(/\s+/g, " ")
      .trim();
  const match = (actual: unknown, wanted: any, exact = false) =>
    typeof wanted === "string"
      ? exact
        ? norm(actual) === wanted
        : norm(actual).includes(wanted)
      : new RegExp(wanted.source, wanted.flags).test(norm(actual));
  const hidden = (el: Element) =>
    !el.checkVisibility() || !!el.closest('[aria-hidden="true"]');
  const text = (node: Node): string => {
    if (node.nodeType === Node.TEXT_NODE) return (node as Text).data;
    if (!(node instanceof Element) || /^(SCRIPT|STYLE)$/.test(node.tagName))
      return "";
    const style = getComputedStyle(node);
    if (node.ariaHidden === "true" || style.display === "none") return "";
    const own = node.ariaLabel || [...node.childNodes].map(text).join("");
    return style.display.startsWith("inline") ? own : ` ${own} `;
  };
  const implicit: Record<string, string> = {
    BUTTON: "button",
    DIALOG: "dialog",
    FIELDSET: "group",
    SELECT: "combobox",
    SUMMARY: "button",
  };
  const inputs: Record<string, string> = {
    button: "button",
    checkbox: "checkbox",
    hidden: "",
    radio: "radio",
    submit: "button",
  };
  const role = (el: Element) => {
    const explicit = el.getAttribute("role")?.split(" ")[0];
    if (explicit) return explicit;
    if (/^H[1-6]$/.test(el.tagName)) return "heading";
    if (el.tagName === "A") return el.hasAttribute("href") ? "link" : "";
    if (el instanceof HTMLInputElement) return inputs[el.type] ?? "textbox";
    if (el.tagName === "SECTION") return el.ariaLabel ? "region" : "";
    return implicit[el.tagName] ?? "";
  };
  const fromContent =
    /^(button|cell|checkbox|heading|link|menuitem|option|radio|switch|tab|tooltip)$/;
  const name = (el: Element) => {
    const ids = el.getAttribute("aria-labelledby")?.split(/\s+/);
    if (ids)
      return ids
        .map((id) => document.getElementById(id))
        .map((target) => (target ? text(target) : ""))
        .join(" ");
    if (el.ariaLabel) return el.ariaLabel;
    const legend = el.tagName === "FIELDSET" && el.querySelector("legend");
    if (legend) return text(legend);
    const labels = (el as HTMLInputElement).labels;
    if (labels?.length) return [...labels].map(text).join(" ");
    if (fromContent.test(role(el)) && norm(text(el))) return text(el);
    return el.getAttribute("title") ?? "";
  };
  const within = (roots: Element[], keep: (el: Element) => boolean) =>
    roots.flatMap((root) =>
      [...root.querySelectorAll("*")].filter((el) => keep(el) && !hidden(el)),
    );
  let nodes: Element[] = [document.documentElement];
  for (const step of steps) {
    if (step.kind === "css")
      nodes = nodes
        .flatMap((root) => [...root.querySelectorAll(step.value)])
        .filter(
          (el) => step.text === undefined || match(el.textContent, step.text),
        );
    else if (step.kind === "role")
      nodes = within(
        nodes,
        (el) =>
          role(el) === step.role &&
          (step.name === undefined || match(name(el), step.name, step.exact)),
      );
    else nodes = nodes.slice(step.index, step.index + 1);
  }
  return nodes;
}

function firstElement(elements: Element[]) {
  if (!elements[0]) throw new Error("no element");
  return elements[0];
}

async function retry<T>(check: () => Promise<T>, timeout = 5000) {
  const end = Date.now() + timeout;
  while (true) {
    try {
      return await check();
    } catch (error) {
      if (Date.now() >= end) throw error;
      await Bun.sleep(40);
    }
  }
}

export class Locator {
  constructor(
    readonly page: Page,
    readonly steps: Step[],
  ) {}
  private with(step: Step) {
    return new Locator(this.page, [...this.steps, step]);
  }
  locator(value: string, options: { hasText?: Name } = {}) {
    return this.with({ kind: "css", value, text: options.hasText });
  }
  getByRole(role: string, options: { name?: Name; exact?: boolean } = {}) {
    return this.with({ kind: "role", role, ...options });
  }
  nth(index: number) {
    return this.with({ kind: "nth", index });
  }
  all<T>(fn: string | ((elements: any[], arg?: any) => T), arg?: unknown) {
    return this.page.evaluate<T>(
      `(${fn})((${resolveSteps})(${encode(this.steps)}), ${encode(arg)})`,
    );
  }
  evaluate<T>(fn: (element: any, arg?: any) => T, arg?: unknown): Promise<T> {
    const elements = `(${resolveSteps})(${encode(this.steps)})`;
    return this.page.evaluate<T>(
      `(${fn})((${firstElement})(${elements}), ${encode(arg)})`,
    );
  }
  state(): Promise<ElementState[]> {
    return this.all((elements: HTMLInputElement[]) =>
      elements.map((el) => ({
        text: el.textContent ?? "",
        value: el.value,
        disabled: el.disabled || el.ariaDisabled === "true",
        focused: document.activeElement === el,
        visible: el.checkVisibility() && el.getClientRects().length > 0,
        attrs: Object.fromEntries(
          [...el.attributes].map((attr) => [attr.name, attr.value]),
        ),
      })),
    );
  }
  async click() {
    const point = await retry(() =>
      this.evaluate(async (el: HTMLButtonElement) => {
        el.scrollIntoView({ block: "center", inline: "center" });
        const before = el.getBoundingClientRect();
        await new Promise((done) => requestAnimationFrame(done));
        const box = el.getBoundingClientRect();
        const x = box.x + box.width / 2;
        const y = box.y + box.height / 2;
        const hit = document.elementFromPoint(x, y);
        const covered = !hit || !(el.contains(hit) || hit.contains(el));
        const moved = box.x !== before.x || box.y !== before.y;
        if (box.width === 0 || moved || el.disabled || covered)
          throw new Error(`not actionable: ${hit?.outerHTML.slice(0, 120)}`);
        return { x, y };
      }),
    ).catch((error) => {
      throw new Error(`${error.message} for ${JSON.stringify(this.steps)}`);
    });
    await this.page.raw.click(point.x, point.y);
  }
  async hover() {
    const box = await this.evaluate((el) =>
      el.getBoundingClientRect().toJSON(),
    );
    await this.page.cdp("Input.dispatchMouseEvent", {
      type: "mouseMoved",
      x: box.x + box.width / 2,
      y: box.y + box.height / 2,
    });
  }
  fill(value: string) {
    return this.evaluate((el, text) => {
      el.focus();
      el.value = text;
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
    }, value);
  }
  async textContent() {
    return (await this.state())[0]?.text ?? null;
  }
  async getAttribute(name: string) {
    return (await this.state())[0]?.attrs[name] ?? null;
  }
}

export class Page {
  readonly raw: Bun.WebView;
  readonly errors: string[] = [];
  readonly console: string[] = [];
  private ready: Promise<void> | undefined;
  constructor() {
    this.raw = new Bun.WebView({
      width: 1280,
      height: 800,
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
        this.console.push(`${type}: ${args.map(String).join(" ")}`),
    });
  }
  private init() {
    this.ready ??= (async () => {
      await this.raw.navigate("about:blank");
      await this.raw.cdp("Runtime.enable");
      this.raw.addEventListener("Runtime.exceptionThrown", (event: any) => {
        const details = event.data.exceptionDetails;
        this.errors.push(details.exception?.description ?? details.text);
      });
    })();
    return this.ready;
  }
  locator(value: string, options: { hasText?: Name } = {}) {
    return new Locator(this, []).locator(value, options);
  }
  getByRole(role: string, options: { name?: Name; exact?: boolean } = {}) {
    return new Locator(this, []).getByRole(role, options);
  }
  async cdp<T = any>(method: string, params?: Record<string, unknown>) {
    await this.init();
    return this.raw.cdp<T>(method, params);
  }
  async addInitScript(fn: (arg: any) => unknown, arg?: unknown) {
    await this.cdp("Page.addScriptToEvaluateOnNewDocument", {
      source: `(${fn})(${encode(arg)})`,
    });
  }
  async goto(url: string) {
    await this.init();
    const document = (href: string) => href.split("#")[0];
    // A same-document hash change never fires the load event navigate() awaits.
    if (url.includes("#") && document(url) === document(this.raw.url))
      await this.evaluate((href) => location.assign(href), url);
    else await this.raw.navigate(url);
  }
  reload() {
    return this.raw.reload();
  }
  evaluate<T>(fn: ((arg: any) => T) | string, arg?: unknown): Promise<T> {
    return this.raw.evaluate<T>(
      typeof fn === "string" ? fn : `(${fn})(${encode(arg)})`,
    );
  }
  async setViewportSize(size: { width: number; height: number }) {
    await this.init();
    await this.raw.resize(size.width, size.height);
  }
  async blockRequests(urls: string[]) {
    await this.cdp("Network.enable");
    await this.cdp("Network.setBlockedURLs", { urls });
  }
  async artifact(name: string) {
    await mkdir(artifacts, { recursive: true });
    const stem = resolve(artifacts, name.replace(/[^a-z0-9_.-]+/gi, "-"));
    const shot = await this.raw.screenshot().catch(() => undefined);
    if (shot) await Bun.write(`${stem}.png`, shot);
    const dom = await this.raw
      .evaluate<string>("document.documentElement.outerHTML")
      .catch((error) => String(error));
    const log = [...this.errors, ...this.console].join("\n");
    await Bun.write(`${stem}.txt`, `${this.raw.url}\n\n${log}\n\n${dom}`);
  }
  close() {
    this.raw.close();
  }
}

function locatorExpect(locator: Locator) {
  const assert =
    (ok: (state: ElementState[]) => boolean, message: string) =>
    (options: { timeout?: number } = {}) =>
      retry(async () => {
        const state = await locator.state();
        if (!ok(state))
          throw new Error(
            `${message}: ${JSON.stringify(locator.steps)} ${JSON.stringify(state).slice(0, 800)}`,
          );
      }, options.timeout);
  const text = (state: ElementState[]) =>
    state
      .map((el) => el.text)
      .join(" ")
      .replace(/\s+/g, " ")
      .trim();
  const same = (actual: string | undefined, wanted: Name, exact = true) =>
    typeof wanted === "string"
      ? exact
        ? actual === wanted
        : !!actual?.includes(wanted)
      : wanted.test(actual ?? "");
  return {
    toHaveCount: (count: number, options?: { timeout?: number }) =>
      assert((s) => s.length === count, `count ${count}`)(options),
    toBeVisible: assert((s) => s.some((el) => el.visible), "visible"),
    toBeEnabled: assert((s) => !!s[0] && !s[0].disabled, "enabled"),
    toBeFocused: assert((s) => !!s[0]?.focused, "focused"),
    toHaveAttribute: (name: string, value: Name) =>
      assert((s) => same(s[0]?.attrs[name], value), `attribute ${name}`)(),
    toHaveText: (value: Name, options?: { timeout?: number }) =>
      assert((s) => same(text(s), value), "text")(options),
    toContainText: (value: Name) =>
      assert((s) => same(text(s), value, false), "contains")(),
    toHaveValue: (value: string) =>
      assert((s) => s[0]?.value === value, `value ${value}`)(),
  };
}

const poll = (fn: () => unknown, options: { timeout?: number } = {}) =>
  new Proxy({} as any, {
    get:
      (_target, matcher) =>
      (...args: unknown[]) =>
        retry(async () => {
          (bunExpect(await fn()) as any)[matcher](...args);
        }, options.timeout),
  });

export const expect: any = Object.assign(
  (actual: unknown) =>
    actual instanceof Locator ? locatorExpect(actual) : bunExpect(actual),
  { poll },
);

export function test(name: string, fn: (page: Page) => Promise<unknown>) {
  bunTest(name, async () => {
    const page = new Page();
    try {
      await fn(page);
      if (page.errors.length) throw new Error(page.errors.join("\n"));
    } catch (error) {
      await page.artifact(name).catch(() => {});
      throw error;
    } finally {
      page.close();
    }
  });
}

const axeSource = resolve(
  import.meta.dir,
  "../node_modules/axe-core/axe.min.js",
);
export async function seriousViolations(page: Page, selector = "document") {
  if (!(await page.evaluate("typeof axe === 'object'")))
    await page.evaluate(`(() => { ${await Bun.file(axeSource).text()} })()`);
  const context = selector === "document" ? selector : encode(selector);
  const result = await page.evaluate<{ violations: { impact: string }[] }>(
    `axe.run(${context}, { resultTypes: ["violations"] })`,
  );
  return result.violations.filter(({ impact }) =>
    ["critical", "serious"].includes(impact),
  );
}
