import { expect, test } from "bun:test";
import {
  appRoute,
  closePanel,
  createNavigator,
  openPanel,
  parseRoute,
  serializeRoute,
  withLegal,
  withWorkspace,
  type Route,
} from "./navigation";

const ID = "0f8fad5b-d9cb-469f-a165-70867728950e";

test("hashes parse to routes and serialize to their shortest canonical form", () => {
  for (const [hash, canonical] of [
    ["", "#/"],
    ["#/", "#/"],
    ["#/settings", "#/settings"],
    ["#/endpoint", "#/endpoint"],
    ["#/legal", "#/legal"],
    ["#/settings?dialog=legal", "#/?panels=settings&dialog=legal"],
    ["#/endpoint?panels=settings", "#/?panels=endpoint,settings"],
    [`#/history/${ID}?panels=settings,settings&dialog=legal`, `#/history/${ID}?panels=settings&dialog=legal`],
    ["#/history?panels=bogus,endpoint", "#/history?panels=endpoint"],
    ["#/history?panels=valueOf,settings", "#/history?panels=settings"],
    ["#/?panels=toString", "#/"],
    ["#/constructor", "#/not-found"],
    ["#/__proto__", "#/not-found"],
    ["#/history/", "#/not-found"],
    ["#/history/not-a-uuid", "#/not-found"],
    ["#/history/%E0%A4%A", "#/not-found"],
    ["#/settings/", "#/not-found"],
    ["#/not-found", "#/not-found"],
    [`#/?panels=${"x".repeat(4_096)}`, "#/not-found"],
  ] as const)
    expect(serializeRoute(parseRoute(hash)), hash).toBe(canonical);
  expect(parseRoute("#/endpoint")).toEqual(appRoute({ kind: "measurement" }, ["details"]));
});

test("route changes keep what they do not touch; the last opened panel goes to the end", () => {
  const route = appRoute({ kind: "history", id: ID }, ["settings"], true);
  expect(serializeRoute(withWorkspace(route, { kind: "measurement" }))).toBe("#/?panels=settings&dialog=legal");
  expect(serializeRoute(openPanel(openPanel(route, "details"), "settings"))).toBe(
    `#/history/${ID}?panels=endpoint,settings&dialog=legal`,
  );
  expect(serializeRoute(withLegal(closePanel(route, "settings"), false))).toBe(`#/history/${ID}`);
  expect(serializeRoute(openPanel({ kind: "not-found" }, "details"))).toBe("#/endpoint");
});

function browser(hash = "#/", state: unknown = null) {
  const entries: { hash: string; state: unknown }[] = [{ hash, state }];
  let index = 0;
  const target = new EventTarget();
  let route: Route = appRoute();
  let multiple = true;
  const commits: [string, boolean][] = [];
  const navigator = createNavigator({
    location: {
      get hash() {
        return entries[index]!.hash;
      },
    },
    history: {
      get state() {
        return entries[index]!.state;
      },
      pushState(next, _unused, url) {
        entries.splice(index + 1, Infinity, { hash: String(url), state: next });
        index++;
      },
      replaceState(next, _unused, url) {
        entries[index] = { hash: String(url), state: next };
      },
      back() {
        if (!index) return;
        index--;
        queueMicrotask(() => target.dispatchEvent(new Event("popstate")));
      },
    } as Pick<History, "state" | "pushState" | "replaceState" | "back">,
    target,
    multiple: () => multiple,
    current: () => route,
    commit(next, fromHistory) {
      route = next;
      commits.push([serializeRoute(next), fromHistory]);
    },
  });
  return {
    navigator,
    commits,
    hashes: () => entries.map((entry) => entry.hash),
    get index() {
      return index;
    },
    get route() {
      return route;
    },
    narrow() {
      multiple = false;
    },
    async userBack() {
      index--;
      target.dispatchEvent(new Event("popstate"));
      await Promise.resolve();
    },
  };
}
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test("the first load is canonicalized in place and committed as the browser's navigation", () => {
  const page = browser("#/endpoint?panels=settings");
  page.navigator.start();
  expect(page.hashes()).toEqual(["#/?panels=endpoint,settings"]);
  expect(page.commits).toEqual([["#/?panels=endpoint,settings", true]]);
});

test("a surface the app opened closes by stepping back; one reached by a link closes by replacing", async () => {
  const page = browser();
  page.navigator.start();
  page.navigator.push(openPanel(page.route, "settings"));
  expect(page.hashes()).toEqual(["#/", "#/settings"]);
  page.navigator.close(closePanel(page.route, "settings"));
  expect(serializeRoute(page.route)).toBe("#/");
  await settle();
  expect([page.index, page.hashes()]).toEqual([0, ["#/", "#/settings"]]);

  const linked = browser("#/settings");
  linked.navigator.start();
  linked.navigator.close(closePanel(linked.route, "settings"));
  expect(linked.hashes()).toEqual(["#/"]);
});

test("a panel that replaced another still closes back to where the first was opened", async () => {
  const page = browser();
  page.narrow();
  page.navigator.start();
  page.navigator.push(openPanel(page.route, "settings"));
  page.navigator.replace(openPanel(page.route, "details"));
  expect(page.hashes()).toEqual(["#/", "#/endpoint"]);
  page.navigator.close(closePanel(page.route, "details"));
  await settle();
  expect([page.index, serializeRoute(page.route)]).toEqual([0, "#/"]);
});

test("a route taken while a step back is under way is written after it lands", async () => {
  const page = browser();
  page.navigator.start();
  page.navigator.push(openPanel(page.route, "settings"));
  page.navigator.close(closePanel(page.route, "settings"));
  page.navigator.push(openPanel(page.route, "details"));
  expect(serializeRoute(page.route)).toBe("#/endpoint");
  await settle();
  expect([page.hashes(), page.index]).toEqual([["#/", "#/endpoint"], 1]);
});

test("the browser's own Back re-renders the route it lands on", async () => {
  const page = browser();
  page.navigator.start();
  page.navigator.push(withWorkspace(page.route, { kind: "history", id: null }));
  await page.userBack();
  expect(page.commits.at(-1)).toEqual(["#/", true]);
});

test("where one panel fits, the route keeps the last opened and rewrites the URL without a new entry", () => {
  const page = browser("#/?panels=settings,endpoint");
  page.navigator.start();
  page.narrow();
  page.navigator.refit();
  expect(page.hashes()).toEqual(["#/endpoint"]);
});
