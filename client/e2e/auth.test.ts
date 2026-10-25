import {
  amsterdam,
  baseConfig,
  choosePath,
  closeSettings,
  frankfurt,
  home,
  latencyOnly,
  locked,
  open,
  openSettings,
  password,
  passwordHash,
  phase,
  ready,
  run,
  runButton,
  savedResult,
  spawnPeer,
} from "./fleet";
import { type Locator, Page, expect, test } from "./webview";

async function signIn(page: Page) {
  await page.getByRole("textbox", { name: "Operator password" }).fill(password);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
}

test.chrome(
  "a signed-in protected home runs automatic, HTTP/3 and WebTransport paths",
  async (page) => {
    await open(page, `${locked.url}/login`, {
      servers: [{ id: "self", url: locked.url }],
    });
    await signIn(page);
    for (const [path, kind, protocol] of [
      [/^Automatic/, "fetch-stream", "http/1.1"],
      [/^HTTP\/3/, "fetch-stream", "h3"],
      [/^WebTransport/, "webtransport", "h3"],
    ] as const) {
      const settings = await openSettings(page);
      await choosePath(settings, "Throughput path", path);
      await closeSettings(page);
      await ready(page);
      const saved = await run(page);
      expect(saved.result.multiServer.participants).toEqual(["self"]);
      expect(saved.result.multiServer.failures).toEqual([]);
      expect(saved.result.multiServer.servers[0]!.throughput.transport).toBe(kind);
      const [self] = saved.result.multiServer.servers;
      expect(self!.throughput?.browserProtocol).toBe(protocol);
      expect(saved.result.upload?.reportedBytesPerSec).toBeGreaterThan(0);
    }
  },
);

const feedback = (page: Page, name: string) =>
  page.locator('[aria-label="Settings"] .server-feedback', { hasText: name });

// A server of its own, so the wrong attempt spends none of the shared one's sign-in budget.
test("a wrong password keeps the card's host, show key and focus", async (page) => {
  const vault = await spawnPeer("Vault", (server) => ({
    GM_ADVERTISED_NATIVE_ENDPOINTS: "http1-tls",
    GM_AUTH_MODE: "password",
    GM_AUTH_PUBLIC_URL: server.url,
    GM_AUTH_PASSWORD_HASH: passwordHash,
  }));
  try {
    await open(page, `${vault.server.url}/login`, {
      servers: [{ id: "self", url: vault.server.url }],
    });
    const field = page.getByRole("textbox", { name: "Operator password" });
    await field.fill("not the password");
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("Incorrect password");
    await expect(field).toBeFocused();
    await expect(page.locator("[data-host]")).toHaveText(new URL(vault.server.url).host);
    await page.getByRole("button", { name: "Show password" }).click();
    await expect(field).toHaveAttribute("type", "text");
    await expect(page.getByRole("button", { name: "Hide password" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  } finally {
    vault.kill("SIGKILL");
  }
});

test("signing out stops the page's workers and lands on the signed-out notice, even with a token another sign-in replaced", async (page) => {
  await page.addInitScript(() => {
    const workers = Object.assign(window, { workers: 0 });
    window.Worker = new Proxy(window.Worker, {
      construct(target, args) {
        const worker = Reflect.construct(target, args) as Worker;
        workers.workers++;
        const terminate = worker.terminate.bind(worker);
        worker.terminate = () => {
          worker.terminate = terminate;
          workers.workers--;
          terminate();
        };
        return worker;
      },
    });
    const submit = HTMLFormElement.prototype.submit;
    HTMLFormElement.prototype.submit = function () {
      localStorage.setItem("workers-at-sign-out", String(workers.workers));
      submit.call(this);
    };
  });
  await open(page, `${locked.url}/login`, {
    servers: [{ id: "self", url: locked.url }],
  });
  await signIn(page);
  const signOut = page.getByRole("button", { name: "Sign out Local operator", exact: true });
  await expect(signOut).toBeVisible();
  await expect.poll(() => page.evaluate(() => (window as any).workers)).toBeGreaterThan(0);
  await page.evaluate(() => {
    document.querySelector<HTMLInputElement>('form.account input[name="csrf"]')!.value = "replaced";
  });
  await signOut.click();
  await expect(page.getByRole("status")).toHaveText("You are signed out.");
  expect(await page.evaluate(() => location.search)).toBe("?reason=signed_out");
  expect(await page.evaluate(() => localStorage.getItem("workers-at-sign-out"))).toBe("0");
});

test("a sign-out that would leave a result unsaved asks first, and signs out after staying", async (page) => {
  // History's writes fail, for now, as a busy disk's would: the saver keeps retrying.
  await page.addInitScript(() => {
    const state = { failing: false };
    Object.assign(window, { writes: state });
    const put = IDBObjectStore.prototype.put;
    IDBObjectStore.prototype.put = function (
      this: IDBObjectStore,
      ...args: Parameters<IDBObjectStore["put"]>
    ) {
      if (state.failing && this.name === "results") throw new DOMException("busy", "UnknownError");
      return put.apply(this, args);
    };
  });
  // A server of its own, so this sign-in spends none of the shared one's budget.
  const vault = await spawnPeer("Vault", (server) => ({
    GM_ADVERTISED_NATIVE_ENDPOINTS: "http1-tls",
    GM_AUTH_MODE: "password",
    GM_AUTH_PUBLIC_URL: server.url,
    GM_AUTH_PASSWORD_HASH: passwordHash,
  }));
  try {
    await open(page, `${vault.server.url}/login`, {
      servers: [{ id: "self", url: vault.server.url }],
      config: latencyOnly,
    });
    await signIn(page);
    await ready(page);
    await page.evaluate(() => ((window as any).writes.failing = true));
    await runButton(page, "Start test").click();
    await expect(phase(page, "complete")).toHaveCount(1, { timeout: 20_000 });
    const signOut = page.getByRole("button", { name: "Sign out Local operator", exact: true });
    const ask = page.getByRole("alertdialog", { name: "Sign out without saving?" });
    await signOut.click();
    await expect(ask).toBeVisible({ timeout: 10_000 });
    await ask.getByRole("button", { name: "Stay signed in" }).click();
    await expect(ask).toHaveCount(0);
    // Storage recovers and the result saves; Sign out works again.
    await page.evaluate(() => {
      (window as any).writes.failing = false;
      window.dispatchEvent(new Event("online"));
    });
    await savedResult(page);
    await signOut.click();
    await expect(page.getByRole("status")).toHaveText("You are signed out.");
  } finally {
    vault.kill("SIGKILL");
  }
});

test("a session read that failed at boot is tried again when the network returns", async (page) => {
  await page.addInitScript(() => {
    const state = { unreachable: true, refused: 0 };
    Object.assign(window, { state });
    const original = window.fetch.bind(window);
    window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      if (!state.unreachable || new URL(String(input), location.href).pathname !== "/auth/session")
        return original(input, init);
      state.refused++;
      return Promise.reject(new TypeError("Failed to fetch"));
    }) as typeof fetch;
  });
  // A server of its own, so this sign-in spends none of the shared one's budget.
  const vault = await spawnPeer("Vault", (server) => ({
    GM_ADVERTISED_NATIVE_ENDPOINTS: "http1-tls",
    GM_AUTH_MODE: "password",
    GM_AUTH_PUBLIC_URL: server.url,
    GM_AUTH_PASSWORD_HASH: passwordHash,
  }));
  try {
    await open(page, `${vault.server.url}/login`, {
      servers: [{ id: "self", url: vault.server.url }],
    });
    await signIn(page);
    await expect.poll(() => page.evaluate(() => (window as any).state.refused)).toBeGreaterThan(0);
    const signOut = page.getByRole("button", { name: "Sign out Local operator", exact: true });
    await expect(signOut).toHaveCount(0);
    await page.evaluate(() => {
      (window as any).state.unreachable = false;
      window.dispatchEvent(new Event("online"));
    });
    await expect(signOut).toBeVisible();
  } finally {
    vault.kill("SIGKILL");
  }
});

test("signing out everywhere waits for confirmation", async (page) => {
  await open(page, `${locked.url}/login`, {
    servers: [{ id: "self", url: locked.url }],
  });
  await signIn(page);
  const everywhere = page.getByRole("button", {
    name: "Sign out Local operator everywhere",
  });
  await expect(everywhere).toBeVisible();
  // Record the submission instead of ending the fleet's shared operator sessions.
  await page.evaluate(() => {
    HTMLFormElement.prototype.submit = function () {
      (window as any).submitted = new FormData(this).get("scope") ?? "this session";
    };
  });
  const confirm = page.getByRole("alertdialog", {
    name: "Sign out everywhere?",
  });
  await everywhere.click();
  await confirm.getByRole("button", { name: "Stay signed in" }).click();
  expect(await page.evaluate(() => (window as any).submitted)).toBeUndefined();
  await everywhere.click();
  await confirm.getByRole("button", { name: "Sign out everywhere" }).click();
  await expect.poll(() => page.evaluate(() => (window as any).submitted)).toBe("all");
});

test("an HTTP interface explains a protected HTTPS refusal", async (page) => {
  const refusal = await fetch(`${locked.url}/preflight`, {
    headers: { Origin: home.http },
    tls: { rejectUnauthorized: false },
  });
  expect(refusal.status).toBe(403);
  expect(refusal.headers.get("Graphite-Meter-Auth")).toBe("required");
  expect(refusal.headers.get("Access-Control-Allow-Origin")).toBeNull();
  await page.addInitScript((unmarked) => {
    const original = window.fetch.bind(window);
    window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), location.href);
      return url.pathname === "/preflight" && url.origin === unmarked
        ? Promise.resolve(new Response(null, { status: 403 }))
        : original(input, init);
    }) as typeof fetch;
  }, amsterdam.url);
  const servers = [{ id: "self", url: home.http }, frankfurt, amsterdam, locked];
  await open(page, home.http, { servers });
  const settings = await openSettings(page);
  await expect(feedback(page, "Private")).toContainText(
    "If it requires sign-in, open this interface over HTTPS",
  );
  await expect(settings.getByRole("button", { name: "Retry Private" })).toBeVisible();
  await expect(settings.getByRole("button", { name: /^Sign in/ })).toHaveCount(0);
  await expect(feedback(page, "Amsterdam")).toContainText("Could not check the connection");
  await expect(settings.locator('.server-status[data-state="verified"]')).toHaveCount(2, {
    timeout: 15_000,
  });
});

test("a full remote sign-in offers renewal instead of an approval link", async (page) => {
  await page.addInitScript((peer) => {
    window.open = () => null;
    const original = window.fetch.bind(window);
    window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), location.href);
      return url.origin === peer && url.pathname === "/auth/browser/token"
        ? Promise.resolve(new Response(null, { status: 429 }))
        : original(input, init);
    }) as typeof fetch;
  }, locked.url);
  await open(page, home.url, { servers: [home, locked] });
  await openSettings(page);
  const row = feedback(page, "Private");
  await row.getByRole("button", { name: "Sign in to Private" }).click();
  const renewal = row.getByRole("link", { name: "Renew your sign-in at Private" });
  await expect(renewal).toHaveAttribute("href", `${locked.url}/login`);
  await expect(renewal).toHaveAttribute("rel", "noopener noreferrer");
  await expect(row.getByRole("link", { name: "Open sign-in page" })).toHaveCount(0);
  await row.getByRole("button", { name: "Cancel sign-in" }).click();
  await expect(renewal).toHaveCount(0);
  await expect(row.getByRole("button", { name: "Sign in to Private" })).toBeEnabled();
});

test("a failed approval offers only a new sign-in", async (page) => {
  await page.addInitScript((peer) => {
    window.open = () => null;
    const original = window.fetch.bind(window);
    window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), location.href);
      return url.origin === peer && url.pathname === "/auth/browser/token"
        ? Promise.resolve(new Response(null, { status: 410 }))
        : original(input, init);
    }) as typeof fetch;
  }, locked.url);
  await open(page, home.url, { servers: [home, locked] });
  await openSettings(page);
  const row = feedback(page, "Private");
  await row.getByRole("button", { name: "Sign in to Private" }).click();
  await expect(row.getByRole("button", { name: "Sign in again to Private" })).toHaveText(
    "Sign in again",
  );
  await expect(row).toContainText("The approval's answer was lost on the way. Sign in again.");
  await expect(row.getByRole("link")).toHaveCount(0);
  await expect(row.locator(".approval-code")).toHaveCount(0);
  await expect(row.getByRole("button", { name: "Cancel sign-in" })).toHaveCount(0);
});

test.chrome("peer sign-in opens an isolated popup from the user click", async (page) => {
  await page.addInitScript(() => {
    const open = window.open.bind(window);
    const evidence = { active: false, isolated: false, calls: 0 };
    Object.assign(window, { evidence });
    window.open = (...args) => {
      evidence.active = navigator.userActivation.isActive;
      evidence.calls++;
      const popup = open(...args);
      queueMicrotask(() => (evidence.isolated = popup?.opener === null));
      return popup;
    };
  });
  await open(page, home.url, { servers: [home, locked] });
  await openSettings(page);
  const row = feedback(page, "Private");
  await row.getByRole("button", { name: "Sign in to Private" }).click();
  const link = row.getByRole("link", { name: "Open sign-in page" });
  await expect(link).toBeVisible();
  const challenge = new URL((await link.getAttribute("href"))!).searchParams;
  const popups = async () =>
    (await page.cdp("Target.getTargets")).targetInfos.filter(
      (target: { url: string }) =>
        target.url.startsWith(locked.url) && target.url.includes(challenge.get("challenge")!),
    );
  try {
    await expect.poll(async () => (await popups()).length).toBe(1);
    expect(await page.evaluate(() => (window as any).evidence)).toEqual({
      active: true,
      isolated: true,
      calls: 1,
    });
    await row.getByRole("button", { name: "Cancel sign-in" }).click();
    await expect(link).toHaveCount(0);
  } finally {
    for (const { targetId } of await popups()) await page.cdp("Target.closeTarget", { targetId });
  }
});

/* The approving browser keeps the sign-in session that owns the grant. */
async function approve(row: Locator): Promise<Page> {
  const link = row.getByRole("link", { name: "Open sign-in page" });
  await expect(link).toBeVisible();
  const code = await row.locator(".approval-code strong").textContent();
  expect(code).toMatch(/^[A-Z2-7]{8}$/);
  const approval = new Page();
  try {
    await approval.goto((await link.getAttribute("href"))!);
    await signIn(approval);
    await expect(approval.locator("main")).toContainText(home.url);
    await expect(approval.locator("main")).toContainText(code!);
    await approval.getByRole("button", { name: "Approve this client" }).click();
    return approval;
  } catch (error) {
    await approval.close();
    throw error;
  }
}

test.chrome(
  "a protected WebSocket peer approved through the sign-in link joins a run",
  async (page) => {
    await page.addInitScript(() => (window.open = () => null));
    await open(page, home.url, {
      servers: [home, locked],
    });
    await page.cdp("Network.setCookieControls", {
      enableThirdPartyCookieRestriction: true,
      disableThirdPartyCookieMetadata: true,
      disableThirdPartyCookieHeuristics: true,
    });
    await openSettings(page);
    const row = feedback(page, "Private");
    const signInButton = row.getByRole("button", { name: /^Sign in( again)? to Private$/ });
    await signInButton.click();
    const link = row.getByRole("link", { name: "Open sign-in page" });
    const canceled = await link.getAttribute("href");
    await row.getByRole("button", { name: "Cancel sign-in" }).click();
    await signInButton.click();
    await expect(link).toBeVisible();
    expect(await link.getAttribute("href")).not.toBe(canceled);
    await page.evaluate((origin) => {
      const original = window.fetch.bind(window);
      let outage = true;
      window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), location.href);
        const granted = new Headers(init?.headers).has("Authorization");
        if (!outage || url.origin !== origin || !granted) return original(input, init);
        outage = false;
        return Promise.resolve(new Response(null, { status: 503 }));
      }) as typeof fetch;
    }, locked.url);
    await (await approve(row)).close();
    const retry = row.getByRole("button", { name: "Retry Private" });
    await expect(retry).toBeVisible({ timeout: 10_000 });
    await expect(signInButton).toHaveCount(0);
    await retry.click();
    await expect(row).toHaveCount(0);
    await ready(page);
    await page.cdp("Network.clearBrowserCookies");
    const saved = await run(page);
    expect(saved.result.multiServer.participants).toEqual(["self", "server-4"]);
    expect(saved.result.multiServer.failures).toEqual([]);
    expect(JSON.stringify(saved)).not.toContain("Bearer");
  },
);

test("a peer grant revoked mid-run ends in the sign-in state", async (page) => {
  await page.addInitScript(() => (window.open = () => null));
  await open(page, home.url, {
    servers: [home, locked],
    config: {
      // Wherever the revocation lands, an interval before or after it spans the 800 ms evidence floor; 1.5 s left neither.
      duration: { ...baseConfig.duration, downloadMs: 3000, uploadMs: 1000 },
    },
  });
  await openSettings(page);
  const row = feedback(page, "Private");
  const signInButton = row.getByRole("button", { name: /^Sign in( again)? to Private$/ });
  await signInButton.click();
  const approval = await approve(row);
  let startedAt = 0;
  try {
    await ready(page);
    startedAt = Date.now();
    await runButton(page, "Start test").click();
    await expect(phase(page, "download")).toHaveCount(1, { timeout: 10_000 });
    await approval.evaluate(async () => {
      const { csrf } = await (await fetch("/auth/session")).json();
      const body = new URLSearchParams({ csrf, scope: "all" });
      await fetch("/auth/logout", { method: "POST", body });
    });
  } finally {
    await approval.close();
  }
  const saved = await savedResult(page, startedAt, 20_000);
  expect(saved.result.outcome).toBe("partial");
  const revoked = saved.result.multiServer.failures.find(
    (failure) => failure.scope === "throughput",
  );
  expect(revoked).toMatchObject({
    serverId: "server-4",
    reason: "sign-in-required",
  });
  await expect(phase(page, "complete")).toHaveCount(1);
  await openSettings(page);
  await expect(signInButton).toBeVisible();
});
