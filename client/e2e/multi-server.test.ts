import { fleet, stopFleetServer, test, expect } from "./multi-server-fixtures";
import {
  AxeBuilder,
  expectNoHorizontalOverflow,
  openSettings,
  startTest,
  waitForCompletion,
} from "../browser/webview";
import { isHistoryRecord } from "../src/lib/history/types";
import { configure, savedResult, ready } from "./multi-server-actions";

test("an HTTP page automatically verifies clear and TLS HTTP/1.1 streams", async ({
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
  await expect(
    settings.getByRole("combobox", { name: "Latency measurement servers" }),
  ).toHaveValue("self");
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
  await page.addInitScript(
    (unavailable) => {
      const browser = window as {
        fetch: (
          input: RequestInfo | URL,
          init?: RequestInit,
        ) => Promise<Response>;
      };
      const original = browser.fetch.bind(window);
      browser.fetch = (input, init) => {
        const url = new URL(String(input), location.href);
        if (
          url.pathname === "/probe" &&
          /-\d+$/.test(url.searchParams.get("cb") ?? "") &&
          unavailable.includes(url.origin)
        )
          return Promise.reject(new TypeError("Fixture path unavailable"));
        return original(input, init);
      };
    },
    [fleet[1].url, fleet[2].url, fleet[2].http, fleet[2].h2],
  );
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
  if (saved.multiServer?.failures.length) {
    console.info("Mixed-protocol failures", saved.multiServer.failures);
    console.info(
      "Receiver checkpoint timing",
      await page.evaluate(() =>
        performance
          .getEntriesByType("resource")
          .filter((entry) => entry.name.includes("/upload/checkpoint"))
          .map((entry) => ({
            origin: new URL(entry.name).origin,
            start: entry.startTime,
            duration: entry.duration,
            protocol: (entry as PerformanceResourceTiming).nextHopProtocol,
          })),
      ),
    );
  }
  expect(saved.multiServer?.participants).toHaveLength(4);
  expect(saved.multiServer?.failures).toEqual([]);
  expect(
    saved.multiServer!.servers.map((server) => server.throughput?.origin),
  ).toEqual([fleet[0].url, fleet[1].h2, fleet[2].h3, fleet[3].url]);
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
  const resultSelector = page.getByRole("combobox", {
    name: "Result measurements",
  });
  await resultSelector.press("Home");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(resultSelector).toBeFocused();
  await expect(resultSelector).toHaveValue("self");
  await expect(resultSelector).toContainText("Home");
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await expect(resultSelector).toHaveValue("server-3");
  await page.keyboard.press("Home");
  await page.keyboard.press("Enter");
  await expect(resultSelector).toHaveValue("");
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
    page.locator(".result-server-context").getByRole("option"),
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
  expect(saved.multiServer?.intervals).toEqual([]);
  expect(saved.wireEstimates?.downloadBytesPerSec).toBeGreaterThan(0);
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

test("switching a verified fleet to self starts immediately", async ({
  page,
}) => {
  await configure(page, ["self", "server-1"]);
  await ready(page);
  const settings = await openSettings(page);
  await settings.getByRole("checkbox", { name: "Frankfurt" }).click();
  await settings.getByRole("button", { name: "Close Settings" }).click();
  await startTest(page);
  await waitForCompletion(page, 30000);
  const saved = await savedResult(page);
  expect(saved.multiServer?.participants).toEqual(["self"]);
  expect(saved.multiServer?.failures).toEqual([]);
  expect(saved.stages.upload.result?.reportedBytesPerSec).toBeGreaterThan(0);
});

test("a live download refuses an upload stage without receiver checkpoint support", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const browser = window as {
      fetch: (
        input: RequestInfo | URL,
        init?: RequestInit,
      ) => Promise<Response>;
    };
    const fetch = browser.fetch.bind(window);
    browser.fetch = async (...args) => {
      const response = await fetch(...args);
      if (new URL(String(args[0]), location.href).pathname !== "/preflight")
        return response;
      const body = await response.json();
      body.capabilities.uploadCheckpoint = false;
      const replaced = Response.json(body, {
        status: response.status,
        headers: response.headers,
      });
      Object.defineProperty(replaced, "url", { value: response.url });
      return replaced;
    };
  });
  await configure(page, ["self"], 3000, {
    stages: {
      latency: false,
      download: true,
      upload: false,
      bidirectional: false,
    },
    skipLoadedLatencyWhenStageOff: true,
  });
  await ready(page);
  const startedAt = Date.now();
  await startTest(page);
  await expect(page.locator('[role="status"].label')).toContainText(
    "Downloading",
  );
  const upload = page.getByRole("switch", { name: "Upload stage" });
  await expect(upload).toBeEnabled();
  await upload.click();
  await expect(upload).toHaveAttribute("aria-checked", "false");
  await waitForCompletion(page, 15000);
  const saved = await savedResult(page, startedAt);
  expect(saved.stages.download.result?.reportedBytesPerSec).toBeGreaterThan(0);
  expect(saved.stages.upload.result).toBeNull();
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
    .getByRole("combobox", { name: "Latency measurement servers" })
    .press("Home");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
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
    .getByRole("combobox", { name: "Latency measurement servers" })
    .press("End");
  await page.keyboard.press("Enter");
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
    settings.getByRole("combobox", { name: "Latency measurement servers" }),
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
  await expect(page.locator(".latency-focus select")).toHaveCount(0);
  await page
    .getByRole("combobox", { name: "Result measurements" })
    .press("Home");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
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
  const savedScope = page
    .locator(".saved-server-context")
    .getByRole("combobox", { name: "Result measurements" });
  await savedScope.press("Home");
  await page.keyboard.press("Enter");
  await expect(savedScope).toHaveValue("");
  await expect(page.locator(".latency-empty")).toHaveCount(0);
  await expect(
    page.getByRole("combobox", { name: "Saved latency server" }),
  ).toHaveCount(0);
  await savedScope.click();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(savedScope).toHaveValue("self");
  await expect(page.locator(".latency-empty")).toBeVisible();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await expect(savedScope).toHaveValue("server-1");
  await expect(page.locator(".latency-empty")).toHaveCount(0);
  await expect(
    page.locator('[data-latency-profile][data-variant="compact"]'),
  ).toBeVisible();
  await page.artifact("primary-latency-history");
});
