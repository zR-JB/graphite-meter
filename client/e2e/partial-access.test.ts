import {
  test,
  expect,
  openSettings,
  startTest,
  waitForCompletion,
  type Page,
} from "../browser/webview";
import { createServerFleet, type FleetServer } from "./server-fleet";
import { configureFleet, ready, savedResult } from "./server-fleet-actions";
import { isHistoryRecord } from "../src/lib/history/types";

const { fleet } = await createServerFleet({ mutualCatalogue: true });
const configure = (
  page: Page,
  ids: string[],
  servers = fleet,
  pageOrigin = fleet[0].url,
) =>
  configureFleet(servers, page, ids, 1500, config, undefined, pageOrigin, true);
const config = {
  transports: {
    throughputTarget: "protocol:http1",
    latencyTarget: "transport:websocket",
  },
} as const;
type FailureMode = "failed" | "hanging";
type NetworkState = {
  catalogues: string[];
  preflights: Record<string, number>;
  mode: FailureMode;
};

// Catalogues and successful measurement traffic use real independent servers.
// Only the browser's unavailable routes are simulated; this does not exercise
// operating-system routing failures or private-network permission prompts.
async function limitReachability(
  page: Page,
  reachable: FleetServer[],
  mode: FailureMode,
) {
  await page.addInitScript(
    ({ blocked, mode }) => {
      const state: NetworkState = { catalogues: [], preflights: {}, mode };
      Object.assign(window, { partialAccess: state });
      const browser = window as {
        fetch: (
          input: RequestInfo | URL,
          init?: RequestInit,
        ) => Promise<Response>;
      };
      const original = browser.fetch.bind(window);
      browser.fetch = (input, init) => {
        const url = new URL(
          input instanceof Request ? input.url : String(input),
          location.href,
        );
        if (url.pathname === "/servers") state.catalogues.push(url.origin);
        if (url.pathname === "/preflight")
          state.preflights[url.origin] =
            (state.preflights[url.origin] ?? 0) + 1;
        if (!blocked.includes(url.origin)) return original(input, init);
        if (state.mode === "failed")
          return Promise.reject(new TypeError("Failed to fetch"));
        return new Promise<Response>((_resolve, reject) => {
          const signal =
            init?.signal ??
            (input instanceof Request ? input.signal : undefined);
          const abort = () =>
            reject(signal?.reason ?? new DOMException("Aborted", "AbortError"));
          if (signal?.aborted) abort();
          else signal?.addEventListener("abort", abort, { once: true });
        });
      };
    },
    {
      blocked: fleet
        .filter((server) => !reachable.includes(server))
        .flatMap((server) => [server.http, server.url, server.h2, server.h3]),
      mode,
    },
  );
}
async function networkState(page: Page) {
  return page.evaluate(
    () =>
      (window as typeof window & { partialAccess: NetworkState }).partialAccess,
  );
}
async function selectedIds(page: Page) {
  return page.evaluate(() =>
    JSON.parse(localStorage.getItem("graphite-meter:server-selection:v1")!).map(
      (server: { id: string }) => server.id,
    ),
  );
}
function choices(settings: Awaited<ReturnType<typeof openSettings>>) {
  return settings.getByRole("group", { name: "Servers to test", exact: true });
}
async function completeAndReload(page: Page, ids: string[]) {
  const started = Date.now();
  await startTest(page);
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, started);
  expect(isHistoryRecord(saved)).toBe(true);
  expect(saved.outcome).toBe("complete");
  expect(saved.multiServer?.participants).toEqual(ids);
  expect(saved.multiServer?.failures).toEqual([]);
  for (const stage of ["download", "upload"] as const)
    expect(saved.stages[stage].result).not.toBeNull();
  expect(saved.stages.bidirectional.down).not.toBeNull();
  expect(saved.stages.bidirectional.up).not.toBeNull();
  for (const server of saved.multiServer!.servers) {
    expect(server.totalBytes.down).toBeGreaterThan(0);
    expect(server.totalBytes.up).toBeGreaterThan(0);
    expect(server.latencyByStage.latency?.probeCount).toBeGreaterThan(0);
  }
  if (ids.length > 1)
    for (const stage of ["download", "upload", "bidirectional"] as const) {
      const interval = saved.multiServer!.intervals.find(
        (interval) => interval.stage === stage,
      )!;
      expect(interval.complete).toBe(true);
      expect(interval.participants).toEqual(ids);
    }
  const settings = await openSettings(page);
  await settings.getByRole("link", { name: "View History" }).click();
  await page.locator("a.result-row").first().click();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  expect((await networkState(page)).catalogues).toEqual([
    await page.evaluate(() => location.origin),
  ]);
  await page.reload();
  await expect(page.locator(".result-detail")).toBeVisible();
  await expect(page.locator(".saved-servers-section li")).toHaveCount(
    ids.length,
  );
  expect((await savedResult(page)).multiServer).toEqual(saved.multiServer);
  expect(await selectedIds(page)).toEqual(ids);
}

test("all four real nodes list the other three in their catalogues", async () => {
  for (const server of fleet) {
    const response = await fetch(`${server.http}/servers`);
    expect(response.ok).toBe(true);
    const catalogue = (await response.json()) as {
      servers: { id: string; url: string }[];
    };
    expect(catalogue.servers).toHaveLength(4);
    expect(
      catalogue.servers
        .filter((entry) => entry.id !== "self")
        .map((entry) => entry.url)
        .sort(),
    ).toEqual(
      fleet
        .filter((peer) => peer !== server)
        .map((peer) => peer.url)
        .sort(),
    );
  }
});

for (const reachableCount of [1, 2])
  for (const mode of ["failed", "hanging"] as const)
    test(`a mutual four-node catalogue runs with ${reachableCount} reachable and ${mode} unselected peers`, async ({
      page,
    }) => {
      const reachable = fleet.slice(0, reachableCount);
      const ids = reachable.map((server) => server.id);
      await limitReachability(page, reachable, mode);
      await configure(page, ids);
      await ready(page);
      const settings = await openSettings(page);
      await expect(choices(settings).getByRole("checkbox")).toHaveCount(4);
      await expect
        .poll(() => choices(settings).getAttribute("aria-busy"), {
          timeout: 15000,
        })
        .toBe("false");
      expect(await selectedIds(page)).toEqual(ids);
      const settled = (await networkState(page)).preflights;
      await Bun.sleep(mode === "hanging" ? 5500 : 600);
      expect((await networkState(page)).preflights).toEqual(settled);
      await settings.getByRole("button", { name: "Close Settings" }).click();
      await completeAndReload(page, ids);
      expect((await networkState(page)).catalogues).toEqual([fleet[0].url]);
    });

test("a reachable selected pair starts while unselected metadata checks are hanging", async ({
  page,
}) => {
  const ids = ["self", fleet[1].id];
  await limitReachability(page, fleet.slice(0, 2), "hanging");
  await configure(page, ids);
  await ready(page);
  const settings = await openSettings(page);
  await expect(choices(settings)).toHaveAttribute("aria-busy", "true");
  await settings.getByRole("button", { name: "Close Settings" }).click();
  const started = Date.now();
  await startTest(page);
  await expect(page.locator('.gauge-panel[data-phase="latency"]')).toBeVisible({
    timeout: 2000,
  });
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page, started);
  expect(saved.outcome).toBe("complete");
  expect(saved.multiServer?.participants).toEqual(ids);
  expect(saved.multiServer?.failures).toEqual([]);
});

for (const mode of ["failed", "hanging"] as const)
  test(`a saved ${mode} peer stays selected until the user cancels, retries and deselects it`, async ({
    page,
  }) => {
    const ids = ["self", fleet[1].id];
    await limitReachability(page, [fleet[0]], mode);
    await configure(page, ids);
    expect(await selectedIds(page)).toEqual(ids);
    await startTest(page);
    if (mode === "hanging") {
      await expect(
        page.getByRole("button", { name: "Cancel", exact: true }),
      ).toBeVisible();
      await page.getByRole("button", { name: "Cancel", exact: true }).click();
      await expect(
        page.getByRole("button", { name: "Start the speed test", exact: true }),
      ).toBeVisible();
      await expect(page.locator(".gauge-status.error")).toHaveCount(0);
      expect(await selectedIds(page)).toEqual(ids);
      await startTest(page);
    }
    await expect(page.locator(".gauge-status.error")).toBeVisible({
      timeout: 10000,
    });
    await expect(page.locator(".gauge-notes")).toContainText("Frankfurt");
    const settings = await openSettings(page);
    await expect(
      settings.getByRole("button", { name: "Retry Frankfurt", exact: true }),
    ).toBeVisible({ timeout: 15000 });
    await expect(settings.locator(".server-feedback")).toContainText(
      mode === "hanging" ? "timed out" : "could not be reached",
    );
    await expect(settings.getByRole("button", { name: /Sign in/ })).toHaveCount(
      0,
    );
    const before = (await networkState(page)).preflights[fleet[1].url];
    const retryStarted = Date.now();
    await settings
      .getByRole("button", { name: "Retry Frankfurt", exact: true })
      .click();
    if (mode === "hanging") {
      await expect(
        settings.locator('.readiness-badge[data-state="checking"]'),
      ).toBeVisible();
      await expect(
        choices(settings).getByRole("checkbox", { name: /^Frankfurt,/ }),
      ).toBeEnabled();
    }
    await expect
      .poll(async () => (await networkState(page)).preflights[fleet[1].url], {
        timeout: 15000,
      })
      .toBeGreaterThan(before);
    if (mode === "hanging") {
      const admissionMs = Date.now() - retryStarted;
      // Two active metadata jobs have five-second deadlines. Leave scheduling
      // tolerance while rejecting another full round of unselected work.
      expect(admissionMs).toBeLessThan(7500);
      page.console.push(`Selected retry admitted after ${admissionMs} ms`);
      await page.artifact("partial-access-hanging-retry");
    }
    expect(await selectedIds(page)).toEqual(ids);
    await choices(settings)
      .getByRole("checkbox", { name: /^Frankfurt,/ })
      .click();
    await settings.getByRole("button", { name: "Close Settings" }).click();
    await ready(page);
    expect(await selectedIds(page)).toEqual(["self"]);
    await completeAndReload(page, ["self"]);
  });

test("opening another reachable node uses its local self and does not traverse mutual catalogues", async ({
  page,
}) => {
  const home = fleet[1];
  const localFleet = [home, ...fleet.filter((server) => server !== home)].map(
    (server) => ({
      ...server,
      id:
        server === home
          ? "self"
          : server.id === "self"
            ? "home-peer"
            : server.id,
    }),
  );
  await limitReachability(page, fleet.slice(0, 2), "failed");
  await configure(page, ["self", "home-peer"], localFleet, home.url);
  await ready(page);
  const settings = await openSettings(page);
  await expect(choices(settings).getByRole("checkbox")).toHaveCount(4);
  await expect(
    choices(settings).getByRole("checkbox", { name: /^Frankfurt,/ }),
  ).toBeChecked();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await completeAndReload(page, ["self", "home-peer"]);
  expect((await networkState(page)).catalogues).toEqual([home.url]);
  expect(
    (await savedResult(page)).multiServer?.servers.find(
      (server) => server.server.id === "self",
    )?.server.url,
  ).toBe(home.url);
});
