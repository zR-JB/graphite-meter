import { expect, test } from "bun:test";
import { install, leftThisPage } from "./pending";

// Just the DOM the script touches: elements found by tag, class, id and attribute.
let focused: El | null = null;
class El {
  parent: El | null = null;
  readonly attributes: Map<string, string>;
  textContent = "";
  hidden: boolean;
  disabled = false;
  type: string;
  value: string;
  tabIndex = 0;
  constructor(
    readonly tag: string,
    attributes: Record<string, string>,
    readonly children: El[],
  ) {
    this.attributes = new Map(Object.entries(attributes));
    this.hidden = "hidden" in attributes;
    this.type = attributes.type ?? "";
    this.value = attributes.value ?? "";
    for (const child of children) child.parent = this;
  }
  get action() {
    return new URL(this.attributes.get("action")!, "https://meter.example/login").href;
  }
  matches(selector: string): boolean {
    if (selector.startsWith("[")) return this.attributes.has(selector.slice(1, -1));
    if (selector.startsWith("#")) return this.attributes.get("id") === selector.slice(1);
    const [tag, name] = selector.split(".");
    return this.tag === tag && (!name || this.attributes.get("class") === name);
  }
  querySelectorAll(selector: string): El[] {
    return this.children.flatMap((child) => [
      ...(child.matches(selector) ? [child] : []),
      ...child.querySelectorAll(selector),
    ]);
  }
  querySelector(selector: string) {
    return this.querySelectorAll(selector)[0] ?? null;
  }
  closest(selector: string): El | null {
    return this.matches(selector) ? this : (this.parent?.closest(selector) ?? null);
  }
  getAttribute(name: string) {
    return this.attributes.get(name) ?? null;
  }
  setAttribute(name: string, value: string) {
    this.attributes.set(name, value);
  }
  hasAttribute(name: string) {
    return this.attributes.has(name);
  }
  removeAttribute(name: string) {
    this.attributes.delete(name);
  }
  replaceWith(next: El) {
    const siblings = this.parent!.children;
    siblings[siblings.indexOf(this)] = next;
    next.parent = this.parent;
  }
  before(node: El) {
    const siblings = this.parent!.children;
    siblings.splice(siblings.indexOf(this), 0, node);
    node.parent = this.parent;
  }
  get previousElementSibling(): El | null {
    const siblings = this.parent!.children;
    return siblings[siblings.indexOf(this) - 1] ?? null;
  }
  focus() {
    focused = this;
  }
}
const h = (tag: string, attributes: Record<string, string> = {}, ...children: El[]) =>
  new El(tag, attributes, children);

const signIn = (action = "/auth/password") =>
  h(
    "main",
    { class: "card" },
    h("h1"),
    h("p", { "data-host": "" }),
    h(
      "form",
      { action },
      h("input", { name: "csrf", value: "token" }),
      h("input", { id: "password", name: "password", type: "password", value: "pass word&" }),
      h("button", { type: "button", "data-reveal": "password", hidden: "" }, h("svg")),
      h("button", { type: "submit" }),
    ),
  );
const done = () => h("main", { class: "card" }, h("h1"), h("p"));

type Reply = { redirected?: boolean; url: string; ok?: boolean; status?: number; card?: El };

function load(card: El, reply: () => Promise<Reply> = () => new Promise(() => {})) {
  focused = null;
  const root = h("body", {}, card);
  const listeners = new Map<string, (event: object) => void>();
  const shown = new Map<string, (event: object) => void>();
  const effects: string[] = [];
  const posts: [string, RequestInit][] = [];
  const document = {
    title: "Sign in",
    get activeElement() {
      return focused;
    },
    addEventListener: (type: string, listener: (event: object) => void) => listeners.set(type, listener),
    querySelector: (selector: string) => root.querySelector(selector),
    querySelectorAll: (selector: string) => root.querySelectorAll(selector),
    getElementById: (id: string) => root.querySelector(`#${id}`),
    importNode: (node: El) => node,
    createElement: (tag: string) => h(tag),
  };
  let answer: Reply | undefined;
  // A clock the test moves by hand.
  let now = 0;
  const timers = new Map<number, { at: number; fn: () => void }>();
  let ids = 0;
  const advance = (ms: number) => {
    now += ms;
    for (const [id, timer] of [...timers].sort((a, b) => a[1].at - b[1].at))
      if (timer.at <= now) {
        timers.delete(id);
        timer.fn();
      }
  };
  install({
    document,
    location: {
      host: "meter.example",
      href: "https://meter.example/login",
      origin: "https://meter.example",
      pathname: "/login",
      assign: (url: string) => effects.push(`assign ${url}`),
      reload: () => effects.push("reload"),
    },
    fetch: async (url: URL, init: RequestInit) => {
      posts.push([url.href, init]);
      const aborted = new Promise<never>((_, reject) =>
        init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError"))),
      );
      answer = await Promise.race([reply(), aborted]);
      return { redirected: false, ok: true, status: 200, text: async () => "", ...answer };
    },
    DOMParser: class {
      parseFromString = () => ({ title: "Next", querySelector: () => answer!.card ?? null });
    },
    FormData: class extends FormData {
      constructor(form: El) {
        super();
        for (const input of form.querySelectorAll("input")) this.append(input.getAttribute("name")!, input.value);
      }
    },
    addEventListener: (type: string, listener: (event: object) => void) => shown.set(type, listener),
    setTimeout: (fn: () => void, ms: number) => {
      timers.set(++ids, { at: now + ms, fn });
      return ids;
    },
    clearTimeout: (id: number) => timers.delete(id),
  } as unknown as Parameters<typeof install>[0]);
  const fire = (type: string, target: El) => {
    let prevented = false;
    listeners.get(type)!({ target, preventDefault: () => (prevented = true) });
    return prevented;
  };
  const restore = (persisted: boolean) => shown.get("pageshow")!({ persisted });
  return {
    root,
    document,
    effects,
    posts,
    fire,
    restore,
    advance,
    settled: () => new Promise((done) => setImmediate(done)),
  };
}

test("only a redirect to another page leaves this page", () => {
  for (const [redirected, url, pathname, left] of [
    [true, "https://meter.example/login?error=password", "/login", false],
    [true, "https://meter.example/", "/login", true],
    [true, "https://meter.example/auth/cli?challenge=abc", "/login", true],
    [false, "https://meter.example/auth/cli/approve", "/auth/cli", false],
  ] as const)
    expect(leftThisPage({ redirected, url }, { pathname })).toBe(left);
});

test("the card shows its host, and the show key reveals the password without taking its focus", () => {
  const card = signIn();
  const { document, fire } = load(card);
  const key = card.querySelector("[data-reveal]")!;
  const field = document.getElementById("password")!;
  expect([card.querySelector("[data-host]")!.textContent, key.hidden]).toEqual(["meter.example", false]);

  expect(fire("pointerdown", key.children[0]!)).toBe(false);
  field.focus();
  expect(fire("pointerdown", key.children[0]!)).toBe(true);
  fire("click", key.children[0]!);
  expect([field.type, key.getAttribute("aria-pressed"), key.getAttribute("aria-label")]).toEqual([
    "text",
    "true",
    "Hide password",
  ]);
  fire("click", key);
  expect([field.type, key.getAttribute("aria-pressed"), key.getAttribute("aria-label")]).toEqual([
    "password",
    "false",
    "Show password",
  ]);
});

test("a sign-in posts in place, then swaps in the answer's card or follows it off the page", async () => {
  const cases: { reply: Reply; effects: string[]; focus?: "password" | "heading" }[] = [
    { reply: { redirected: true, url: "https://meter.example/" }, effects: ["assign https://meter.example/"] },
    {
      reply: { redirected: true, url: "https://meter.example/login?error=password", card: signIn() },
      effects: [],
      focus: "password",
    },
    { reply: { url: "https://meter.example/auth/cli/approve", card: done() }, effects: [], focus: "heading" },
    { reply: { url: "https://meter.example/auth/password" }, effects: ["assign https://meter.example/auth/password"] },
    { reply: { url: "https://meter.example/auth/password", ok: false, status: 403 }, effects: ["reload"] },
  ];
  for (const { reply, effects, focus } of cases) {
    const card = signIn();
    const page = load(card, async () => reply);
    const form = card.querySelector("form")!;
    const field = page.document.getElementById("password")!;
    page.fire("click", card.querySelector("[data-reveal]")!);

    expect(page.fire("submit", form)).toBe(true);
    expect(field.type).toBe("password");
    expect(form.hasAttribute("data-busy")).toBe(true);
    expect(form.querySelectorAll("button").map((button) => button.disabled)).toEqual([true, true]);
    const [[url, init]] = page.posts as [[string, RequestInit]];
    expect([url, init.method, String(init.body), init.credentials, init.redirect]).toEqual([
      "https://meter.example/auth/password",
      "POST",
      "csrf=token&password=pass+word%26",
      "same-origin",
      "follow",
    ]);

    await page.settled();
    expect(page.effects).toEqual(effects);
    const shown = page.root.children[0]!;
    expect(shown).toBe(reply.card ?? card);
    if (!reply.card) continue;
    expect(page.document.title).toBe("Next");
    const heading = shown.querySelector("h1")!;
    if (focus === "password") {
      expect([shown.querySelector("[data-host]")!.textContent, shown.querySelector("[data-reveal]")!.hidden]).toEqual([
        "meter.example",
        false,
      ]);
      expect(focused).toBe(shown.querySelector("#password"));
    } else expect(focused).toBe(heading);
    expect(heading.tabIndex).toBe(focus === "heading" ? -1 : 0);
  }
});

test("a post that cannot reach the server says so once and leaves the form usable", async () => {
  for (const reply of [
    () => Promise.reject(new TypeError("offline")),
    async () => ({ url: "https://meter.example/auth/password", ok: false, status: 502 }),
  ]) {
    const card = signIn();
    const page = load(card, reply);
    const form = card.querySelector("form")!;
    page.fire("submit", form);
    await page.settled();
    page.fire("submit", form);
    await page.settled();
    expect([form.hasAttribute("data-busy"), ...form.querySelectorAll("button").map((each) => each.disabled)]).toEqual([
      false,
      false,
      false,
    ]);
    const notices = card.querySelectorAll("[data-unreachable]");
    expect(notices.map((each) => [each.textContent, each.getAttribute("role"), each.getAttribute("class")])).toEqual([
      ["Could not reach the server. Try again.", "alert", "notice"],
    ]);
    expect([form.previousElementSibling, page.effects]).toEqual([notices[0]!, []]);
  }
});

test("a post that gets no answer frees its form after 20 s, says the outcome is unknown and posts nothing again", async () => {
  const card = signIn();
  const page = load(card);
  const form = card.querySelector("form")!;
  page.fire("submit", form);
  await page.settled();
  page.advance(19_999);
  await page.settled();
  expect(form.hasAttribute("data-busy")).toBe(true);
  page.advance(1);
  await page.settled();
  expect([form.hasAttribute("data-busy"), ...form.querySelectorAll("button").map((each) => each.disabled)]).toEqual([
    false,
    false,
    false,
  ]);
  expect(card.querySelectorAll("[data-unreachable]").map((each) => each.textContent)).toEqual([
    "The server did not answer in time. Reload to see whether it went through, or try again.",
  ]);
  expect(page.posts).toHaveLength(1);
});

test("a busy form takes no second submit, until the page returns from the back-forward cache", () => {
  const waiting = load(signIn());
  const busy = waiting.root.querySelector("form")!;
  waiting.fire("submit", busy);
  expect(waiting.fire("submit", busy)).toBe(true);
  expect(waiting.posts).toHaveLength(1);
  waiting.restore(false);
  expect(busy.hasAttribute("data-busy")).toBe(true);
  waiting.restore(true);
  expect([busy.hasAttribute("data-busy"), ...busy.querySelectorAll("button").map((each) => each.disabled)]).toEqual([
    false,
    false,
    false,
  ]);
  waiting.fire("submit", busy);
  expect(waiting.posts).toHaveLength(2);
});

test("forms bound elsewhere submit natively, busy", () => {
  for (const action of ["/auth/oidc/start", "https://idp.example/auth/password"]) {
    const card = signIn(action);
    const page = load(card);
    const form = card.querySelector("form")!;
    expect(page.fire("submit", form)).toBe(false);
    expect([page.posts.length, form.hasAttribute("data-busy")]).toEqual([0, true]);
  }
});
