import {
  closeSettings,
  fleet,
  frankfurt,
  helsinki,
  home,
  locked,
  open,
  openSettings,
} from "./fleet";
import { expect, test, type Page } from "./webview";

interface Activity {
  requests: string[];
  catalogs: string[];
  inFlight: number;
  peak: number;
  probes: number;
  workers: number;
  activeWorkers: number;
  hold: string[];
  aborted: number;
  nowOffset: number;
}

async function observe(page: Page, hold: string[] = []) {
  await page.addInitScript(
    (activity: Activity) => {
      Object.assign(window, { activity });
      const now = Date.now.bind(Date);
      Date.now = () => now() + activity.nowOffset;
      const original = window.fetch.bind(window);
      window.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), location.href);
        if (url.pathname === "/probe") activity.probes++;
        if (url.pathname === "/servers") activity.catalogs.push(url.origin);
        if (url.pathname !== "/preflight") return original(input, init);
        activity.requests.push(url.origin);
        activity.peak = Math.max(activity.peak, ++activity.inFlight);
        try {
          if (activity.hold.includes(url.origin))
            await new Promise((_resolve, reject) => {
              init?.signal?.addEventListener("abort", () => {
                activity.aborted++;
                reject(init.signal!.reason);
              });
            });
          return await original(input, init);
        } finally {
          activity.inFlight--;
        }
      }) as typeof fetch;
      window.Worker = new Proxy(window.Worker, {
        construct(target, args) {
          activity.workers++;
          activity.activeWorkers++;
          const worker = Reflect.construct(target, args) as Worker;
          const terminate = worker.terminate.bind(worker);
          let active = true;
          worker.terminate = () => {
            if (active) activity.activeWorkers--;
            active = false;
            terminate();
          };
          return worker;
        },
      });
    },
    {
      requests: [],
      catalogs: [],
      inFlight: 0,
      peak: 0,
      probes: 0,
      workers: 0,
      activeWorkers: 0,
      hold,
      aborted: 0,
      nowOffset: 0,
    },
  );
  return {
    read: () => page.evaluate<Activity>(() => (window as any).activity),
    set: (change: Partial<Activity>) =>
      page.evaluate((next) => {
        const activity = (window as any).activity as Activity;
        Object.assign(activity, next, {
          nowOffset: activity.nowOffset + (next.nowOffset ?? 0),
        });
      }, change),
  };
}

test("settings discovery is bounded, reused and cancelled on close", async (page) => {
  const activity = await observe(page);
  await open(page, home.url, {
    config: {
      transports: { throughputTarget: "protocol:http1", latencyTarget: "auto" },
    },
    latency: { mode: "all", serverId: "self" },
  });
  const settings = await openSettings(page);
  const choices = settings.getByRole("group", { name: "Servers to test" });
  const ready = settings.locator('[data-readiness="verified"]');
  await expect(ready).toBeVisible({ timeout: 15_000 });
  await expect(choices.locator(".server-preflight")).toHaveCount(4);
  await expect(choices).toHaveAttribute("aria-busy", "false");
  const first = await activity.read();
  expect(first.requests).toEqual(fleet.map((server) => server.url));
  expect(first.peak).toBeLessThanOrEqual(4);
  expect([first.probes, first.workers, first.activeWorkers]).toEqual([2, 1, 1]);
  await closeSettings(page);
  await openSettings(page);
  expect(await activity.read()).toEqual(first);

  const peer = choices.getByRole("checkbox", { name: /^Frankfurt/ });
  await peer.click();
  await expect(ready).toBeVisible({ timeout: 15_000 });
  await expect.poll(async () => (await activity.read()).activeWorkers).toBe(1);
  const selected = await activity.read();
  expect(selected.requests).toEqual(first.requests);
  expect([selected.probes, selected.workers]).toEqual([4, 2]);
  await peer.click();
  await closeSettings(page);

  await activity.set({ nowOffset: 121_000 });
  await openSettings(page);
  await expect
    .poll(async () => (await activity.read()).requests.length)
    .toBe(8);
  await expect(choices).toHaveAttribute("aria-busy", "false");
  const refreshed = await activity.read();
  const signIn = refreshed.requests.filter((origin) => origin === locked.url);
  expect(signIn).toHaveLength(1);
  expect([refreshed.probes, refreshed.workers]).toEqual([4, 2]);
  expect(refreshed.peak).toBeLessThanOrEqual(4);

  await closeSettings(page);
  await activity.set({ nowOffset: 121_000, hold: [frankfurt.url] });
  await openSettings(page);
  await expect.poll(async () => (await activity.read()).inFlight).toBe(1);
  await closeSettings(page);
  await expect.poll(async () => (await activity.read()).aborted).toBe(1);
  expect((await activity.read()).catalogs).toEqual([home.url]);
});

test("metadata timeouts back off without starving later servers", async (page) => {
  const activity = await observe(page, [frankfurt.url, fleet[2].url]);
  await open(page);
  const settings = await openSettings(page);
  const choices = settings.getByRole("group", { name: "Servers to test" });
  const status = (name: string) =>
    choices.locator("label", { hasText: name }).locator(".server-status");
  await expect(choices).toHaveAttribute("aria-busy", "true");
  await expect(
    choices
      .locator("label", { hasText: "Helsinki" })
      .locator(".server-preflight"),
  ).toHaveCount(1, { timeout: 15_000 });
  await expect(status("Private")).toHaveText("Sign in", { timeout: 15_000 });
  for (const held of [frankfurt.name, fleet[2].name])
    await expect(status(held)).toHaveText("Unavailable", { timeout: 30_000 });
  const urls = fleet.map((server) => server.url);
  expect((await activity.read()).requests).toEqual(urls);
  await closeSettings(page);
  await openSettings(page);
  await expect(choices).toHaveAttribute("aria-busy", "false");
  expect((await activity.read()).requests).toEqual(urls);
  await expect(choices.locator(".server-preflight")).toHaveCount(2);
  await expect(
    choices.getByRole("checkbox", { name: /^Helsinki/ }),
  ).toBeVisible();
});

test("an origin-only catalog discovers peer identity without traversal", async (page) => {
  const activity = await observe(page);
  await open(page, helsinki.url);
  const settings = await openSettings(page);
  const choices = settings.getByRole("group", { name: "Servers to test" });
  const peer = choices.getByRole("checkbox", { name: /^Frankfurt/ });
  await expect(peer).toBeVisible({ timeout: 15_000 });
  expect(await peer.evaluate((input) => input.checked)).toBe(false);
  await choices.locator("label", { hasText: "Frankfurt" }).hover();
  await expect(page.getByRole("tooltip")).toContainText("Loopback fixture");
  expect((await activity.read()).catalogs).toEqual([helsinki.url]);
});
