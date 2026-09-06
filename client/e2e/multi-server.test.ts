import "./client-performance";
import {
  fleet,
  fixturePassword,
  stopFleetServer,
  test,
  expect,
} from "./multi-server-fixtures";
import {
  Page,
  AxeBuilder,
  openSettings,
  startTest,
  waitForCompletion,
  expectNoHorizontalOverflow,
} from "../browser/webview";
import { isHistoryRecord } from "../src/lib/history/types";

import { configure, savedResult, ready } from "./multi-server-actions";

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
  await page.addInitScript((peer) => {
    localStorage.setItem(
      "graphite-meter:v1",
      JSON.stringify({ latencySelection: { mode: "all", serverId: "self" } }),
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
        if (state.hold && url.origin === peer) {
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
  }, fleet[1].url);
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
  expect(first.peak).toBe(1);
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
  await expect.poll(async () => (await activity()).requests.length).toBe(9);
  await expect(choices).toHaveAttribute("aria-busy", "false");
  const refreshed = await activity();
  expect(refreshed.workers).toBe(2);
  expect(refreshed.probes).toBe(4);
  expect(refreshed.peak).toBe(1);

  // Closing Settings aborts the current fetch and prevents the rest of the queue from starting.
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await page.evaluate(() => {
    const state = (
      window as typeof window & { discoveryActivity: DiscoveryActivity }
    ).discoveryActivity;
    state.nowOffset += 121_000;
    state.hold = true;
  });
  await openSettings(page);
  await expect.poll(async () => (await activity()).inFlight).toBe(1);
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await expect.poll(async () => (await activity()).aborted).toBe(1);
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
  expect(await requests()).toEqual(
    fleet.slice(0, 3).map((server) => server.url),
  );
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await openSettings(page);
  await expect(choices).toHaveAttribute("aria-busy", "false");
  expect(await requests()).toEqual(fleet.map((server) => server.url));
  await expect(choices.locator(".server-preflight")).toHaveCount(2);
  await expect(
    choices.getByRole("checkbox", { name: "Helsinki, Loopback fixture" }),
  ).toBeVisible();
});

test("an HTTP page automatically uses its clear server and a TLS-only peer while listing all preflight times", async ({
  page,
}) => {
  // An ordinary non-loopback HTTP page does not expose WebTransport. Exercise
  // the same fallback with real clear and TLS listeners in the local fixture.
  await page.addInitScript(() => {
    Object.defineProperty(window, "WebTransport", {
      value: undefined,
      configurable: true,
    });
  });
  await configure(
    page,
    ["self", fleet[1].id],
    1500,
    {
      transports: { throughputTarget: "auto", latencyTarget: "auto" },
    },
    { mode: "primary", serverId: "self" },
    fleet[0].http,
  );
  await ready(page);
  const settings = await openSettings(page);
  const choices = settings.getByRole("group", {
    name: "Servers to test",
    exact: true,
  });
  await expect(choices).toHaveAttribute("aria-busy", "false");
  await expect(choices.locator(".server-preflight")).toHaveCount(4);
  await expect(settings.getByRole("radio", { name: "Home" })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await settings.getByRole("button", { name: "Close Settings" }).click();
  const startedAt = Date.now();
  await startTest(page);
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, startedAt);
  expect(isHistoryRecord(saved)).toBe(true);
  expect(saved.multiServer?.failures).toEqual([]);
  const [home, peer] = saved.multiServer!.servers;
  expect(home.server.url).toBe(fleet[0].http);
  expect(home.throughput?.origin).toBe(fleet[0].http);
  expect(peer.server.url).toBe(fleet[1].url);
  expect(peer.throughput?.origin).toBe(fleet[1].url);
  expect(home.latencyTarget?.transport).toBe("websocket");
  expect(peer.latencyTarget).toBeNull();
  for (const server of [home, peer]) {
    expect(server.totalBytes.down).toBeGreaterThan(0);
    expect(server.totalBytes.up).toBeGreaterThan(0);
  }
});

test("four real servers share one run and retain separate receiver windows and latency after reload", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await configure(
    page,
    fleet.slice(0, 4).map((server) => server.id),
  );
  await ready(page);
  const selectionSettings = await openSettings(page);
  await expect(
    selectionSettings
      .getByRole("group", { name: "Servers to test", exact: true })
      .getByRole("checkbox")
      .first(),
  ).toBeEnabled();
  await selectionSettings
    .getByRole("button", { name: "Close Settings" })
    .click();
  const startedAt = Date.now();
  await startTest(page);
  await openSettings(page);
  await expect(
    selectionSettings
      .getByRole("group", { name: "Servers to test", exact: true })
      .getByRole("checkbox")
      .first(),
  ).toBeDisabled();
  await selectionSettings
    .getByRole("button", { name: "Close Settings" })
    .click();
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, startedAt);
  expect(isHistoryRecord(saved)).toBe(true);
  expect(saved.schemaVersion).toBe(4);
  expect(saved.multiServer?.participants).toHaveLength(4);
  expect(saved.multiServer?.failures).toEqual([]);
  for (const stage of ["download", "upload", "bidirectional"] as const) {
    const interval = saved.multiServer!.intervals.find(
      (interval) => interval.stage === stage,
    )!;
    expect(interval.complete).toBe(true);
    expect(interval.participants).toHaveLength(4);
    for (const dir of stage === "bidirectional"
      ? (["down", "up"] as const)
      : ([stage === "download" ? "down" : "up"] as const))
      expect(interval.headline?.[dir]).toHaveLength(4);
  }
  for (const server of saved.multiServer!.servers) {
    expect(server.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
    expect(server.totalBytes.down).toBeGreaterThan(0);
    expect(server.totalBytes.up).toBeGreaterThan(0);
  }
  const resultSelector = page.getByRole("radiogroup", {
    name: "Result measurements",
  });
  await resultSelector.getByRole("radio", { name: /^Combined,/ }).focus();
  await page.keyboard.press("ArrowRight");
  await expect(
    resultSelector.getByRole("radio", { name: "Home" }),
  ).toBeFocused();
  await expect(
    resultSelector.getByRole("radio", { name: "Home" }),
  ).toHaveAttribute("aria-checked", "true");
  await expect(page.getByRole("tooltip")).toContainText("Loopback fixture");
  await page.keyboard.press("End");
  await expect(
    resultSelector.getByRole("radio", { name: "Helsinki" }),
  ).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("Home");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("radio", { name: /^Combined,/ })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  const audit = await new AxeBuilder({ page })
    .include(".results-slot")
    .analyze();
  expect(audit.violations).toEqual([]);
  await page.artifact("multi-server-desktop-result");
  const settings = await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").click();
  await expect(page.locator(".result-server-context")).toBeVisible();
  await expect(
    page.locator('.result-server-context [role="radio"]'),
  ).toHaveCount(5);
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await page.locator(".result-server-context").scrollIntoViewIfNeeded();
  await page.artifact("multi-server-history-desktop");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.locator(".result-server-context").scrollIntoViewIfNeeded();
  await expectNoHorizontalOverflow(page.locator(".result-server-context"));
  await page.artifact("multi-server-history-phone");
  const servers = page.locator(".saved-servers-section");
  await servers.scrollIntoViewIfNeeded();
  await expect(servers.locator("li")).toHaveCount(4);
  await expect(servers).toContainText("Loopback fixture");
  await expect(servers).toContainText(new URL(fleet[1].url).host);
  expect(
    await servers.evaluate((section) =>
      section.nextElementSibling?.classList.contains("detail-actions"),
    ),
  ).toBe(true);
  await expectNoHorizontalOverflow(servers);
  await page.artifact("saved-servers-phone");
  await page.reload();
  await expect(page.locator(".result-server-context")).toBeVisible();
  expect((await savedResult(page)).multiServer?.intervals).toEqual(
    saved.multiServer?.intervals,
  );
});

test("a single-server result keeps the ordinary live and history views in a fleet", async ({
  page,
}) => {
  await configure(page, ["self"]);
  await ready(page);
  await expect(page.locator(".server-indicator")).toHaveCount(0);
  const startedAt = Date.now();
  await startTest(page);
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, startedAt);
  expect(saved.multiServer?.selection).toHaveLength(1);
  await expect(page.locator(".result-server-context")).toHaveCount(0);
  await expect(page.locator(".server-indicator")).toHaveCount(0);
  const settings = await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").click();
  await expect(page.locator(".result-detail")).toBeVisible();
  await expect(page.locator(".result-server-context")).toHaveCount(0);
  await expect(page.locator(".server-focus")).toHaveCount(0);
  await expect(page.locator(".saved-servers-section li")).toHaveCount(1);
  await expect(page.locator(".saved-servers-section")).toContainText(
    "Home · Loopback fixture",
  );
  await page.artifact("single-server-fleet-history");
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
  await expect(settings).toContainText("receiver checkpoint support");
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
    {},
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
  expect((await checks()).fetches.map((url) => new URL(url).origin)).toEqual([
    fleet[1].url,
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
  expect(added.fetches.slice(4).map((url) => new URL(url).origin)).toEqual([
    fleet[3].url,
    fleet[3].http,
  ]);
  expect(added.workers).toBe(3);
});

test("a real peer dropout keeps healthy transfers running and persists its failure after reload", async ({
  page,
}) => {
  await configure(
    page,
    fleet.slice(0, 3).map((server) => server.id),
    1500,
    {
      // Leave time for bounded lane recovery, terminal removal, and a fresh survivor window.
      duration: {
        warmupMs: 250,
        latencyMs: 1000,
        downloadMs: 14000,
        uploadMs: 1500,
        bidirectionalMs: 1500,
      },
    },
  );
  await ready(page);
  const startedAt = Date.now();
  await startTest(page);
  await expect(page.locator('[role="status"].label')).toContainText(
    "Downloading",
    {
      timeout: 10000,
    },
  );
  await Bun.sleep(700);
  await stopFleetServer("server-2");
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, startedAt);
  expect(isHistoryRecord(saved)).toBe(true);
  expect(saved.outcome).toBe("partial");
  expect(saved.multiServer?.participants).toEqual(["self", "server-1"]);
  expect(
    saved.multiServer?.failures.find(
      (failure) => failure.scope === "throughput",
    ),
  ).toMatchObject({
    serverId: "server-2",
    scope: "throughput",
    stage: "download",
  });
  const subsequent = saved.multiServer!.intervals.filter(
    (interval) =>
      interval.reason === "dropout" || interval.stage !== "download",
  );
  expect(subsequent.length).toBeGreaterThan(0);
  expect(
    subsequent.every((interval) => !interval.participants.includes("server-2")),
  ).toBe(true);
  expect(saved.stages.upload.result?.reportedBytesPerSec).toBeGreaterThan(0);
  await expect(page.locator(".result-server-context")).toContainText(
    "2 of 3 servers",
  );
  const settings = await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").first().click();
  await page.reload();
  await expect(page.locator(".result-server-context")).toContainText(
    "2 of 3 servers",
  );
  await expect(page.locator(".result-server-context")).toContainText(
    "Amsterdam",
  );
  expect((await savedResult(page)).multiServer?.failures).toEqual(
    saved.multiServer?.failures,
  );
  await page.artifact("multi-server-partial-history");
});

test("a full remote login offers explicit renewal and discards the old approval link", async ({
  page,
}) => {
  await page.addInitScript((peer) => {
    window.open = () => null;
    const original = window.fetch.bind(window);
    window.fetch = ((input, init) => {
      const url = new URL(String(input), location.href);
      return url.origin === peer && url.pathname === "/auth/browser/token"
        ? Promise.resolve(new Response(null, { status: 429 }))
        : original(input, init);
    }) as typeof window.fetch;
  }, fleet[4].url);
  await configure(page, ["self", fleet[4].id]);
  await openSettings(page);
  const row = page.locator(".server-feedback", { hasText: "Private" });
  await row.getByRole("button", { name: "Sign in to Private" }).click();
  const renewal = row.getByRole("link", { name: "Renew login at Private" });
  await expect(renewal).toBeVisible();
  await expect(renewal).toHaveAttribute("href", `${fleet[4].url}/login`);
  await expect(renewal).toHaveAttribute("rel", "noopener noreferrer");
  await expect(row).toContainText(
    "Renewing ends the other client connections authorized by that login",
  );
  await expect(
    row.getByRole("link", { name: "Open sign-in page" }),
  ).toHaveCount(0);
  await row.getByRole("button", { name: "Cancel sign-in" }).click();
  await expect(renewal).toHaveCount(0);
  await expect(
    row.getByRole("button", { name: "Sign in to Private" }),
  ).toBeEnabled();
});

for (const transport of ["websocket", "webtransport"] as const)
  test(`protected peer ${transport} approval works without third-party cookies and with the popup fallback`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.addInitScript(() => {
      window.open = () => null;
    });
    await configure(page, ["self", fleet[4].id], 1800, {
      transports: {
        throughputTarget:
          transport === "webtransport" ? "transport:webtransport" : "auto",
        latencyTarget: `transport:${transport}`,
      },
    });
    await page.raw.cdp("Network.setCookieControls", {
      enableThirdPartyCookieRestriction: true,
      disableThirdPartyCookieMetadata: true,
      disableThirdPartyCookieHeuristics: true,
    });
    await openSettings(page);
    const row = page.locator(".server-feedback", { hasText: "Private" });
    await expect(
      row.getByRole("button", { name: "Sign in to Private" }),
    ).toBeVisible({ timeout: 15000 });
    await row.getByRole("button", { name: "Sign in to Private" }).click();
    const link = row.getByRole("link", { name: "Open sign-in page" });
    await expect(link).toBeVisible();
    const cancelledURL = await link.getAttribute("href");
    await row.getByRole("button", { name: "Cancel sign-in" }).click();
    await expect(link).toHaveCount(0);
    await row.getByRole("button", { name: "Sign in to Private" }).click();
    await expect(link).toBeVisible();
    expect(await link.getAttribute("href")).not.toBe(cancelledURL);
    const comparisonCode = await row
      .locator(".approval-code strong")
      .textContent();
    expect(comparisonCode).toMatch(/^[A-Z2-7]{8}$/);
    await page.evaluate((origin) => {
      const fetch = window.fetch.bind(window);
      let failOnce = true;
      window.fetch = ((input, init) => {
        const url = new URL(
          input instanceof Request ? input.url : String(input),
          location.href,
        );
        if (
          failOnce &&
          url.origin === origin &&
          url.pathname === "/preflight" &&
          new Headers(init?.headers).has("Authorization")
        ) {
          failOnce = false;
          return Promise.resolve(
            Response.json({ error: "Temporary outage" }, { status: 503 }),
          );
        }
        return fetch(input, init);
      }) as typeof window.fetch;
    }, fleet[4].url);
    const approval = new Page();
    try {
      await approval.goto((await link.getAttribute("href"))!);
      await approval.getByLabel("Operator password").fill(fixturePassword);
      await approval
        .getByRole("button", { name: "Sign in with operator password" })
        .click();
      await expect(
        approval.getByRole("heading", { name: "Approve browser client" }),
      ).toBeVisible();
      await expect(approval.locator("main")).toContainText(fleet[0].url);
      await expect(approval.locator("main")).toContainText(comparisonCode!);
      await approval
        .getByRole("button", { name: "Approve this client" })
        .click();
      // A connection error after approval must keep the accepted grant and offer
      // a path retry, instead of incorrectly asking the user to sign in again.
      await expect(
        row.getByRole("button", { name: "Retry Private" }),
      ).toBeVisible();
      await expect(
        row.getByRole("button", { name: "Sign in to Private" }),
      ).toHaveCount(0);
      await row.getByRole("button", { name: "Retry Private" }).click();
      await expect(row).toHaveCount(0);
      await ready(page);
      // Every WebView shares Chromium's cookie jar. Remove cookies locally without
      // logging out the parent session; the requesting page must rely on its grant.
      await page.raw.cdp("Network.clearBrowserCookies");
      const cookies = await page.raw.cdp<{ cookies: unknown[] }>(
        "Network.getCookies",
        { urls: [fleet[4].url] },
      );
      expect(cookies.cookies).toEqual([]);
      const startedAt = Date.now();
      await startTest(page);
      await waitForCompletion(page, 30000);
      const saved = await savedResult(page, startedAt);
      expect(isHistoryRecord(saved)).toBe(true);
      expect(saved.multiServer?.participants).toEqual(["self", fleet[4].id]);
      expect(saved.multiServer?.failures).toEqual([]);
      expect(saved.multiServer?.servers[1].totalBytes.up).toBeGreaterThan(0);
      expect(JSON.stringify(saved)).not.toContain("Bearer");
    } finally {
      approval.close();
    }
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

test("primary latency selection is fixed for the run and saved alongside every throughput participant", async ({
  page,
}) => {
  await configure(page, ["self", "server-1"]);
  await ready(page);
  // Already verified latency choices must not open another discovery or ping worker.
  await page.evaluate(
    (origins) => {
      const state = globalThis as typeof globalThis & {
        delayedPreflights: number;
      };
      state.delayedPreflights = 0;
      const browser = window as {
        fetch: (
          input: RequestInfo | URL,
          init?: RequestInit,
        ) => Promise<Response>;
      };
      const original = browser.fetch;
      browser.fetch = async (...args) => {
        if (
          String(args[0]).includes("/preflight?") &&
          origins.includes(new URL(String(args[0]), location.href).origin) &&
          state.delayedPreflights < 2
        ) {
          state.delayedPreflights++;
          await new Promise((resolve) => setTimeout(resolve, 600));
        }
        return original(...args);
      };
    },
    [fleet[0].url, fleet[1].url],
  );
  const settings = await openSettings(page);
  await settings
    .getByRole("radiogroup", { name: "Latency measurement servers" })
    .getByRole("radio", { name: "Home" })
    .click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (globalThis as typeof globalThis & { delayedPreflights: number })
            .delayedPreflights,
      ),
    )
    .toBe(0);
  await settings
    .getByRole("radiogroup", { name: "Latency measurement servers" })
    .getByRole("radio", { name: "Frankfurt" })
    .click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          JSON.parse(localStorage.getItem("graphite-meter:v1")!)
            .latencySelection.serverId,
      ),
    )
    .toBe("server-1");
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await ready(page);
  const started = Date.now();
  await startTest(page);
  await openSettings(page);
  await expect(
    settings
      .getByRole("radiogroup", { name: "Latency measurement servers" })
      .getByRole("radio", { name: /All servers/ }),
  ).toBeDisabled();
  await expect(
    settings
      .getByRole("radiogroup", { name: "Latency measurement servers" })
      .getByRole("radio", { name: "Home" }),
  ).toBeDisabled();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, started);
  expect(isHistoryRecord(saved)).toBe(true);
  expect(saved.multiServer?.failures).toEqual([]);
  expect(saved.multiServer?.participants).toEqual(["self", "server-1"]);
  const home = saved.multiServer!.servers.find(
    (server) => server.server.id === "self",
  )!;
  const primary = saved.multiServer!.servers.find(
    (server) => server.server.id === "server-1",
  )!;
  expect(home.latencyTarget).toBeNull();
  expect(home.latency).toBeNull();
  expect(
    Object.values(home.latencyByStage).every((value) => value === null),
  ).toBe(true);
  expect(home.totalBytes.down).toBeGreaterThan(0);
  expect(home.totalBytes.up).toBeGreaterThan(0);
  expect(primary.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
  expect(primary.latencyByStage.download?.probeCount).toBeGreaterThan(0);
  expect(saved.multiServer?.latencyFocus).toBe("server-1");
  await expect(page.locator(".latency-focus .server-tag")).toHaveAttribute(
    "aria-label",
    /Frankfurt/,
  );
  await expect(page.locator(".latency-focus [role=radio]")).toHaveCount(0);
  await page
    .getByRole("radiogroup", { name: "Result measurements" })
    .getByRole("radio", { name: "Home" })
    .click();
  await expect(page.locator(".result-cards")).toContainText("Not measured");
  await expect(page.locator(".latency-focus .server-tag")).toHaveAttribute(
    "aria-label",
    /Frankfurt/,
  );
  await page.artifact("primary-latency-result");
  await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").first().click();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await expect(page.locator(".saved-server-context")).toBeVisible();
  await page
    .locator(".saved-server-context")
    .getByRole("radiogroup", { name: "Result measurements" })
    .getByRole("radio", { name: "Frankfurt" })
    .click();
  await expect(
    page
      .locator(".saved-server-context")
      .getByRole("radio", { name: "Frankfurt" }),
  ).toHaveAttribute("aria-checked", "true");
  await page.artifact("primary-latency-history");
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
