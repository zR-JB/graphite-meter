import {
  amsterdam,
  baseConfig,
  closeSettings,
  frankfurt,
  home,
  locked,
  open,
  openSettings,
  password,
  phase,
  ready,
  run,
  runButton,
  savedResult,
} from "./fleet";
import { Locator, Page, expect, test } from "./webview";

async function signIn(page: Page) {
  await page.getByRole("textbox", { name: "Operator password" }).fill(password);
  await page
    .getByRole("button", { name: "Sign in with operator password" })
    .click();
}

test("a signed-in protected home runs automatic, HTTP/3 and WebTransport paths", async (page) => {
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
    await settings
      .getByRole("group", { name: "Throughput path" })
      .getByRole("radio", { name: path })
      .click();
    await closeSettings(page);
    await ready(page);
    const saved = await run(page);
    expect(saved.multiServer?.participants).toEqual(["self"]);
    expect(saved.multiServer?.failures).toEqual([]);
    expect(saved.transport.throughput.kind).toBe(kind);
    const [self] = saved.multiServer!.servers;
    expect(self.throughput?.browserProtocol).toBe(protocol);
    expect(saved.stages.upload.result?.reportedBytesPerSec).toBeGreaterThan(0);
  }
});

const feedback = (page: Page, name: string) =>
  page.locator('[aria-label="Settings"] .server-feedback', { hasText: name });

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
  const servers = [
    { id: "self", url: home.http },
    frankfurt,
    amsterdam,
    locked,
  ];
  await open(page, home.http, { servers });
  const settings = await openSettings(page);
  await expect(feedback(page, "Private")).toContainText(
    "If it requires sign-in, open this interface over HTTPS",
  );
  await expect(
    settings.getByRole("button", { name: "Retry Private" }),
  ).toBeVisible();
  await expect(settings.getByRole("button", { name: /^Sign in/ })).toHaveCount(
    0,
  );
  await expect(feedback(page, "Amsterdam")).toContainText(
    "Connection check failed",
  );
  await expect(
    settings.locator('.server-status[data-state="ready"]'),
  ).toHaveCount(2, { timeout: 15_000 });
});

test("a full remote login offers renewal instead of an approval link", async (page) => {
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
  const renewal = row.getByRole("link", { name: "Renew login at Private" });
  await expect(renewal).toHaveAttribute("href", `${locked.url}/login`);
  await expect(renewal).toHaveAttribute("rel", "noopener noreferrer");
  await expect(
    row.getByRole("link", { name: "Open sign-in page" }),
  ).toHaveCount(0);
  await row.getByRole("button", { name: "Cancel sign-in" }).click();
  await expect(renewal).toHaveCount(0);
  await expect(
    row.getByRole("button", { name: "Sign in to Private" }),
  ).toBeEnabled();
});

test("peer sign-in opens an isolated popup from the user click", async (page) => {
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
        target.url.startsWith(locked.url) &&
        target.url.includes(challenge.get("challenge")!),
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
    for (const { targetId } of await popups())
      await page.cdp("Target.closeTarget", { targetId });
  }
});

/* The approving browser keeps the login session that owns the grant. */
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
    approval.close();
    throw error;
  }
}

test("a protected WebSocket peer approved through the sign-in link joins a run", async (page) => {
  await page.addInitScript(() => (window.open = () => null));
  await open(page, home.url, {
    servers: [home, locked],
    latency: { mode: "all", serverId: "self" },
  });
  await page.cdp("Network.setCookieControls", {
    enableThirdPartyCookieRestriction: true,
    disableThirdPartyCookieMetadata: true,
    disableThirdPartyCookieHeuristics: true,
  });
  await openSettings(page);
  const row = feedback(page, "Private");
  const signInButton = row.getByRole("button", { name: "Sign in to Private" });
  await signInButton.click();
  const link = row.getByRole("link", { name: "Open sign-in page" });
  const cancelled = await link.getAttribute("href");
  await row.getByRole("button", { name: "Cancel sign-in" }).click();
  await signInButton.click();
  await expect(link).toBeVisible();
  expect(await link.getAttribute("href")).not.toBe(cancelled);
  await page.evaluate((origin) => {
    const original = window.fetch.bind(window);
    let outage = true;
    window.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), location.href);
      const granted = new Headers(init?.headers).has("Authorization");
      if (!outage || url.origin !== origin || !granted)
        return original(input, init);
      outage = false;
      return Promise.resolve(new Response(null, { status: 503 }));
    }) as typeof fetch;
  }, locked.url);
  (await approve(row)).close();
  const retry = row.getByRole("button", { name: "Retry Private" });
  await expect(retry).toBeVisible({ timeout: 10_000 });
  await expect(signInButton).toHaveCount(0);
  await retry.click();
  await expect(row).toHaveCount(0);
  await ready(page);
  await page.cdp("Network.clearBrowserCookies");
  const saved = await run(page);
  expect(saved.multiServer?.participants).toEqual(["self", "server-4"]);
  expect(saved.multiServer?.failures).toEqual([]);
  expect(JSON.stringify(saved)).not.toContain("Bearer");
});

test("a peer grant revoked mid-run ends in the sign-in state", async (page) => {
  await page.addInitScript(() => (window.open = () => null));
  await open(page, home.url, {
    servers: [home, locked],
    latency: { mode: "all", serverId: "self" },
    config: {
      duration: { ...baseConfig.duration, downloadMs: 1500, uploadMs: 1000 },
    },
  });
  await openSettings(page);
  const row = feedback(page, "Private");
  const signInButton = row.getByRole("button", { name: "Sign in to Private" });
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
    approval.close();
  }
  const saved = await savedResult(page, startedAt, 20_000);
  expect(saved.outcome).toBe("partial");
  const revoked = saved.multiServer!.failures.find(
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
