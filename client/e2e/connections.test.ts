import { fleet, test, expect } from "./multi-server-fixtures";
import {
  AxeBuilder,
  openSettings,
  expectNoHorizontalOverflow,
} from "../browser/webview";
import { configure, ready } from "./multi-server-actions";

interface DiscoveryActivity {
  requests: string[];
  inFlight: number;
  peak: number;
  probes: number;
  workers: number;
  activeWorkers: number;
  hold: boolean;
  aborted: number;
  nowOffset: number;
}

test("settings and path checks share bounded discovery, cancel on close, and retain only the home ping worker", async ({
  page,
}) => {
  await page.addInitScript((home) => {
    localStorage.setItem(
      "graphite-meter:v1",
      JSON.stringify({
        latencySelection: { mode: "all", serverId: "self" },
        config: {
          transports: {
            throughputTarget: "protocol:http1",
            latencyTarget: "auto",
          },
        },
      }),
    );
    const state: DiscoveryActivity = ((
      window as typeof window & { discoveryActivity: DiscoveryActivity }
    ).discoveryActivity = {
      requests: [],
      inFlight: 0,
      peak: 0,
      probes: 0,
      workers: 0,
      activeWorkers: 0,
      hold: false,
      aborted: 0,
      nowOffset: 0,
    });
    const now = Date.now.bind(Date);
    Date.now = () => now() + state.nowOffset;
    const browser = window as {
      fetch: (
        input: RequestInfo | URL,
        init?: RequestInit,
      ) => Promise<Response>;
    };
    const original = browser.fetch.bind(window);
    browser.fetch = async (input, init) => {
      const url = new URL(String(input), location.href);
      if (url.pathname === "/probe") state.probes++;
      if (url.pathname !== "/preflight") return original(input, init);
      state.requests.push(url.origin);
      state.peak = Math.max(state.peak, ++state.inFlight);
      try {
        if (state.hold && url.origin !== home) {
          await new Promise<void>((_resolve, reject) => {
            const abort = () => {
              state.aborted++;
              reject(init!.signal!.reason);
            };
            if (init?.signal?.aborted) abort();
            else init?.signal?.addEventListener("abort", abort, { once: true });
          });
        }
        return await original(input, init);
      } finally {
        state.inFlight--;
      }
    };
    window.Worker = new Proxy(window.Worker, {
      construct(target, args) {
        state.workers++;
        state.activeWorkers++;
        const worker = Reflect.construct(target, args) as Worker;
        const terminate = worker.terminate.bind(worker);
        let active = true;
        worker.terminate = () => {
          if (active) {
            active = false;
            state.activeWorkers--;
          }
          terminate();
        };
        return worker;
      },
    });
  }, fleet[0].url);
  const activity = () =>
    page.evaluate(
      () =>
        (window as typeof window & { discoveryActivity: DiscoveryActivity })
          .discoveryActivity,
    );
  await page.goto(fleet[0].url);
  const settings = await openSettings(page);
  const choices = settings.getByRole("group", {
    name: "Servers to test",
    exact: true,
  });
  await expect(
    settings.locator('.readiness-badge[data-state="verified"]'),
  ).toBeVisible({ timeout: 15000 });
  await expect(
    choices.getByRole("checkbox", { name: "Helsinki, Loopback fixture" }),
  ).toBeVisible();
  await expect(choices).toHaveAttribute("aria-busy", "false");
  const first = await activity();
  expect(first.requests).toEqual(fleet.map((server) => server.url));
  expect(first.peak).toBeLessThanOrEqual(2);
  expect(first.probes).toBe(2);
  expect(first.workers).toBe(1);
  expect(first.activeWorkers).toBe(1);
  await expect(choices.locator(".server-preflight")).toHaveCount(4);
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await openSettings(page);
  expect(await activity()).toEqual(first);

  // Selecting a discovered peer performs only its role probes, with no second preflight.
  const peer = choices.getByRole("checkbox", { name: "Frankfurt" });
  await peer.click();
  await expect(
    settings.locator('.readiness-badge[data-state="verified"]'),
  ).toBeVisible({ timeout: 15000 });
  await expect.poll(async () => (await activity()).activeWorkers).toBe(1);
  const selected = await activity();
  expect(selected.requests).toEqual(first.requests);
  expect(selected.probes).toBe(4);
  expect(selected.workers).toBe(2);
  await expect(choices.locator(".server-preflight")).toHaveCount(4);
  await peer.click();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await page.evaluate(() => {
    (
      window as typeof window & { discoveryActivity: DiscoveryActivity }
    ).discoveryActivity.nowOffset = 121_000;
  });
  await openSettings(page);
  await expect.poll(async () => (await activity()).requests.length).toBe(8);
  await expect(choices).toHaveAttribute("aria-busy", "false");
  const refreshed = await activity();
  // Authentication failures stay quiet until the user signs in or explicitly retries.
  expect(
    refreshed.requests.filter((origin) => origin === fleet[4].url),
  ).toHaveLength(1);
  expect(refreshed.workers).toBe(2);
  expect(refreshed.probes).toBe(4);
  expect(refreshed.peak).toBeLessThanOrEqual(2);

  // Closing Settings aborts both active fetches and prevents queued metadata from starting.
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await page.evaluate(() => {
    const state = (
      window as typeof window & { discoveryActivity: DiscoveryActivity }
    ).discoveryActivity;
    state.nowOffset += 121_000;
    state.hold = true;
  });
  await openSettings(page);
  await expect.poll(async () => (await activity()).inFlight).toBe(2);
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await expect.poll(async () => (await activity()).aborted).toBe(2);
  const closed = await activity();
  expect(closed.inFlight).toBe(0);
  expect(closed.requests).toHaveLength(10);
  expect(closed.workers).toBe(2);
  await page.evaluate(() => {
    (
      window as typeof window & { discoveryActivity: DiscoveryActivity }
    ).discoveryActivity.hold = false;
  });
  await openSettings(page);
  await expect(choices).toHaveAttribute("aria-busy", "false");
  await peer.click();
  await expect(
    settings.locator('.readiness-badge[data-state="verified"]'),
  ).toBeVisible({ timeout: 15000 });
  await choices.getByRole("checkbox", { name: "Home" }).click();
  await expect.poll(async () => (await activity()).activeWorkers).toBe(0);
  await expect(
    settings.locator('.readiness-badge[data-state="verified"]'),
  ).toBeVisible();
});

test("metadata timeouts back off so later catalogue servers are not starved on reopen", async ({
  page,
}) => {
  await page.addInitScript(
    (heldOrigins) => {
      const state = window as typeof window & { metadataRequests: string[] };
      state.metadataRequests = [];
      const original = window.fetch.bind(window);
      window.fetch = ((input, init) => {
        const url = new URL(String(input), location.href);
        if (url.pathname === "/preflight") {
          state.metadataRequests.push(url.origin);
          if (heldOrigins.includes(url.origin))
            return new Promise<Response>((_resolve, reject) => {
              const abort = () => reject(init!.signal!.reason);
              if (init?.signal?.aborted) abort();
              else
                init?.signal?.addEventListener("abort", abort, { once: true });
            });
        }
        return original(input, init);
      }) as typeof window.fetch;
    },
    [fleet[1].url, fleet[2].url],
  );
  await page.goto(fleet[0].url);
  const settings = await openSettings(page);
  const choices = settings.getByRole("group", {
    name: "Servers to test",
    exact: true,
  });
  await expect(choices).toHaveAttribute("aria-busy", "true");
  await expect
    .poll(() => choices.getAttribute("aria-busy"), { timeout: 15000 })
    .toBe("false");
  const requests = () =>
    page.evaluate(
      () =>
        (window as typeof window & { metadataRequests: string[] })
          .metadataRequests,
    );
  // Timed-out peers release their slots in this opening, without requiring the user to reopen Settings.
  expect(await requests()).toEqual(fleet.map((server) => server.url));
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await openSettings(page);
  await expect(choices).toHaveAttribute("aria-busy", "false");
  expect(await requests()).toEqual(fleet.map((server) => server.url));
  await expect(choices.locator(".server-preflight")).toHaveCount(2);
  await expect(
    choices.getByRole("checkbox", { name: "Helsinki, Loopback fixture" }),
  ).toBeVisible();
});

test("an origin-only catalogue discovers peer identity and paths without repeated configuration", async ({
  page,
}) => {
  await page.goto(fleet[3].url);
  const settings = await openSettings(page);
  const band = settings.getByRole("group", {
    name: "Servers to test",
    exact: true,
  });
  const peer = band.getByRole("checkbox", { name: "Frankfurt" });
  await expect(peer).toBeVisible({ timeout: 15000 });
  expect(await peer.evaluate((input) => input.checked)).toBe(false);
  await peer.click();
  await peer.focus();
  await band.locator("label").nth(1).hover();
  await expect(page.getByRole("tooltip")).toContainText("Loopback fixture");
  await page.artifact("origin-only-peer-discovery");
});

test("server selectors support sliding, keyboard selection, cancellation and narrow layouts", async ({
  page,
}) => {
  await configure(page, ["self", "server-1"]);
  await ready(page);
  const settings = await openSettings(page);
  const selector = settings.getByRole("radiogroup", {
    name: "Latency measurement servers",
  });
  const all = selector.getByRole("radio", { name: /All servers/ });
  const home = selector.getByRole("radio", { name: "Home" });
  const peer = selector.getByRole("radio", { name: "Frankfurt" });
  await all.focus();
  await page.keyboard.press("ArrowRight");
  await expect(home).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("End");
  await expect(peer).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("Home");
  await expect(all).toHaveAttribute("aria-checked", "true");
  const cdp = await page.context.newCDPSession();
  const point = async (option: typeof all) =>
    option.evaluate((element) => {
      const box = element.getBoundingClientRect();
      return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
    });
  const from = await point(all);
  const to = await point(home);
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mousePressed",
    ...from,
    button: "left",
    buttons: 1,
    clickCount: 1,
  });
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mouseMoved",
    ...to,
    buttons: 1,
  });
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mouseReleased",
    ...to,
    button: "left",
    buttons: 0,
    clickCount: 1,
  });
  await expect(home).toHaveAttribute("aria-checked", "true");
  const cancelTo = await point(peer);
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mousePressed",
    ...to,
    button: "left",
    buttons: 1,
    clickCount: 1,
  });
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mouseMoved",
    ...cancelTo,
    buttons: 1,
  });
  await page.keyboard.press("Escape");
  await cdp.send("Input.dispatchMouseEvent", {
    type: "mouseReleased",
    ...cancelTo,
    button: "left",
    buttons: 0,
    clickCount: 1,
  });
  await expect(home).toHaveAttribute("aria-checked", "true");
  await expect(settings).toBeVisible();
  for (const width of [1440, 768, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await expectNoHorizontalOverflow(settings.locator(".panel-body"));
    await expect
      .poll(() =>
        selector.evaluate((element) => {
          const selected = element
            .querySelector('[aria-checked="true"]')!
            .getBoundingClientRect();
          const thumb = element
            .querySelector(".selector-thumb")!
            .getBoundingClientRect();
          return (
            Math.abs(selected.left - thumb.left) +
            Math.abs(selected.top - thumb.top) +
            Math.abs(selected.width - thumb.width) +
            Math.abs(selected.height - thumb.height)
          );
        }),
      )
      .toBeLessThan(1);
  }
  await page.emulateMedia({ reducedMotion: "reduce" });
  await expect
    .poll(() =>
      selector
        .locator(".selector-thumb")
        .evaluate((element) =>
          Number.parseFloat(getComputedStyle(element).transitionDuration),
        ),
    )
    .toBeLessThan(0.0001);
  await all.click();
  await expect(all).toHaveAttribute("aria-checked", "true");
  expect(
    (
      await new AxeBuilder({ page })
        .include('[aria-label="Settings"]')
        .analyze()
    ).violations,
  ).toEqual([]);
  await page.artifact("sliding-server-selector-phone");
});

test("retrying an upgraded server refreshes capability evidence without checking healthy peers", async ({
  page,
}) => {
  await page.addInitScript(
    ({ peer }) => {
      const state = globalThis as typeof globalThis & {
        checkpointsAvailable: boolean;
        preflightOrigins: string[];
      };
      state.checkpointsAvailable = false;
      state.preflightOrigins = [];
      const browser = window as {
        fetch: (
          input: RequestInfo | URL,
          init?: RequestInit,
        ) => Promise<Response>;
      };
      const original = browser.fetch;
      browser.fetch = async (...args) => {
        const response = await original(...args);
        const url = new URL(String(args[0]), location.href);
        if (url.pathname !== "/preflight") return response;
        state.preflightOrigins.push(url.origin);
        if (url.origin !== peer) return response;
        const body = await response.json();
        body.capabilities.uploadCheckpoint = state.checkpointsAvailable;
        const replaced = Response.json(body, {
          status: response.status,
          headers: response.headers,
        });
        Object.defineProperty(replaced, "url", { value: response.url });
        return replaced;
      };
    },
    { peer: fleet[1].url },
  );
  await configure(page, ["self", "server-1"]);
  const settings = await openSettings(page);
  const retry = settings.getByRole("button", { name: "Retry Frankfurt" });
  await expect(retry).toBeVisible();
  await expect(settings).toContainText(
    "Receiver checkpoint support is required for uploads.",
  );
  await expect(settings.locator(".server-choices")).toHaveAttribute(
    "aria-busy",
    "false",
  );
  await page.evaluate(() => {
    const state = globalThis as typeof globalThis & {
      checkpointsAvailable: boolean;
      preflightOrigins: string[];
    };
    state.checkpointsAvailable = true;
    state.preflightOrigins = [];
  });
  await retry.click();
  await expect(
    settings.locator('.readiness-badge[data-state="verified"]'),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        (globalThis as typeof globalThis & { preflightOrigins: string[] })
          .preflightOrigins,
    ),
  ).toEqual([fleet[1].url]);
});

test("enabling all latency checks only the new peer path and retries leave healthy paths open", async ({
  page,
}) => {
  await configure(
    page,
    ["self", "server-1"],
    1500,
    {
      transports: {
        throughputTarget: "protocol:http1",
        latencyTarget: "transport:websocket",
      },
    },
    { mode: "primary", serverId: "self" },
  );
  await ready(page);
  const settings = await openSettings(page);
  await expect(settings.locator(".server-choices")).toHaveAttribute(
    "aria-busy",
    "false",
  );
  await page.evaluate(
    ({ healthy, peer, peerPage }) => {
      const state = globalThis as typeof globalThis & {
        pathChecks: {
          fetches: string[];
          workers: number;
          failPeer: boolean;
          watched: string[];
        };
      };
      state.pathChecks = {
        fetches: [],
        workers: 0,
        failPeer: true,
        watched: [...healthy, peer, peerPage],
      };
      // Idle expiry alone must not turn a new ping choice into a full recheck.
      const now = Date.now.bind(Date);
      Date.now = () => now() + 180_000;
      const browser = window as {
        fetch: (
          input: RequestInfo | URL,
          init?: RequestInit,
        ) => Promise<Response>;
      };
      const original = browser.fetch;
      browser.fetch = async (...args) => {
        const url = new URL(String(args[0]), location.href);
        if (
          (url.pathname === "/preflight" || url.pathname === "/probe") &&
          state.pathChecks.watched.includes(url.origin)
        ) {
          state.pathChecks.fetches.push(url.href);
          if (
            healthy.includes(url.origin) ||
            (url.origin === peer &&
              url.pathname === "/probe" &&
              state.pathChecks.failPeer)
          )
            return new Response(null, { status: 503 });
        }
        return original(...args);
      };
      window.Worker = new Proxy(window.Worker, {
        construct(target, args) {
          state.pathChecks.workers++;
          return Reflect.construct(target, args);
        },
      });
    },
    {
      healthy: [fleet[0].url, fleet[0].http, fleet[0].h2, fleet[0].h3],
      peer: fleet[1].url,
      peerPage: fleet[1].url,
    },
  );
  const selector = settings.getByRole("radiogroup", {
    name: "Latency measurement servers",
  });
  await expect(selector.getByRole("radio", { name: "Home" })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await selector.getByRole("radio", { name: /All servers/ }).click();
  await expect(
    settings.getByRole("button", { name: "Retry Frankfurt" }),
  ).toBeVisible({ timeout: 15000 });
  const checks = () =>
    page.evaluate(
      () =>
        (
          globalThis as typeof globalThis & {
            pathChecks: {
              fetches: string[];
              workers: number;
              failPeer: boolean;
            };
          }
        ).pathChecks,
    );
  // The unchanged server generation is already discovered; only the new latency path is probed.
  expect((await checks()).fetches.map((url) => new URL(url).origin)).toEqual([
    fleet[1].url,
  ]);
  expect((await checks()).workers).toBe(1);
  await page.evaluate(() => {
    (
      globalThis as typeof globalThis & { pathChecks: { failPeer: boolean } }
    ).pathChecks.failPeer = false;
  });
  await settings.getByRole("button", { name: "Retry Frankfurt" }).click();
  await expect(
    settings.locator('.readiness-badge[data-state="verified"]'),
  ).toBeVisible({ timeout: 15000 });
  const verified = await checks();
  // Explicit participant retry refreshes discovery and both enabled paths.
  expect(verified.fetches.map((url) => new URL(url).origin)).toEqual([
    fleet[1].url,
    fleet[1].url,
    fleet[1].url,
    fleet[1].url,
  ]);
  expect(verified.workers).toBe(2);
  for (let cycle = 0; cycle < 3; cycle++) {
    await selector.getByRole("radio", { name: "Home" }).click();
    await selector.getByRole("radio", { name: /All servers/ }).click();
  }
  const choices = settings.getByRole("group", {
    name: "Servers to test",
    exact: true,
  });
  await choices.getByRole("checkbox", { name: "Frankfurt" }).click();
  await choices.getByRole("checkbox", { name: "Frankfurt" }).click();
  await expect(
    settings.locator('.readiness-badge[data-state="verified"]'),
  ).toBeVisible();
  expect(await checks()).toEqual(verified);
  // An unselected server receives no path checks until the user adds it.
  await expect(choices).toHaveAttribute("aria-busy", "false");
  await page.evaluate(
    (origins) => {
      (
        globalThis as typeof globalThis & { pathChecks: { watched: string[] } }
      ).pathChecks.watched.push(...origins);
    },
    [fleet[3].url, fleet[3].http],
  );
  await choices.getByRole("checkbox", { name: "Helsinki" }).click();
  await expect.poll(async () => (await checks()).fetches.length).toBe(6);
  await expect(
    settings.locator('.readiness-badge[data-state="verified"]'),
  ).toBeVisible({ timeout: 15000 });
  const added = await checks();
  // Grouped WebSocket selection keeps the new peer's probe on TLS.
  expect(
    added.fetches
      .slice(verified.fetches.length)
      .map((url) => new URL(url).origin),
  ).toEqual([fleet[3].url, fleet[3].url]);
  expect(added.workers).toBe(3);
});

for (const theme of ["dark", "light"] as const)
  test(`phone server band supports keyboard, ${theme} theme and reduced motion`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.emulateMedia({ reducedMotion: "reduce" });
    await configure(page, ["self"]);
    await ready(page);
    await page.evaluate(
      (value) => document.documentElement.setAttribute("data-theme", value),
      theme,
    );
    const settings = await openSettings(page);
    const band = settings.getByRole("group", {
      name: "Servers to test",
      exact: true,
    });
    await expect(band.getByRole("checkbox", { name: "Home" })).toBeDisabled();
    const peer = band.getByRole("checkbox", { name: "Frankfurt" });
    await peer.focus();
    await page.keyboard.press("Space");
    await expect(peer).toBeChecked();
    await page.keyboard.press("Escape");
    await openSettings(page);
    await expectNoHorizontalOverflow(settings);
    const scan = await new AxeBuilder({ page })
      .include('[aria-label="Settings"]')
      .analyze();
    expect(scan.violations).toEqual([]);
    await page.artifact(`multi-server-phone-${theme}`);
  });

for (const theme of ["light", "dark"] as const)
  test(`${theme} compact server settings fit touch and keyboard use`, async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.setViewportSize({ width: 390, height: 844 });
    await configure(page, ["self", "server-1"]);
    await ready(page);
    await page.evaluate(
      (value) => document.documentElement.setAttribute("data-theme", value),
      theme,
    );
    const settings = await openSettings(page);
    await settings
      .getByRole("radiogroup", { name: "Latency measurement servers" })
      .getByRole("radio", { name: "Home" })
      .click();
    const primary = settings.getByRole("radiogroup", {
      name: "Latency measurement servers",
    });
    await primary.getByRole("radio", { name: "Home" }).focus();
    await page.keyboard.press("ArrowRight");
    await expect(
      primary.getByRole("radio", { name: "Frankfurt" }),
    ).toBeFocused();
    await expect(
      primary.getByRole("radio", { name: "Frankfurt" }),
    ).toHaveAttribute("aria-checked", "true");
    await page.keyboard.press("Escape");
    await openSettings(page);
    await expectNoHorizontalOverflow(settings);
    const audit = await new AxeBuilder({ page })
      .include('[aria-label="Settings"]')
      .analyze();
    expect(audit.violations).toEqual([]);
    await page.artifact(`compact-server-settings-phone-${theme}`);
  });
