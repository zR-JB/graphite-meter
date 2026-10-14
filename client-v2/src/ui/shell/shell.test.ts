import { expect, test } from "bun:test";
import { parseRoute } from "./navigation";
import { focusMove, moves } from "./shell";

const ID = "0f8fad5b-d9cb-469f-a165-70867728950e";
const OTHER = "1f8fad5b-d9cb-469f-a165-70867728950e";
const DOCKS = ["stage", "sheet-left", "sheet-right"];
const PANES = ["history-list", "history-detail"];

test("a route change moves the docks when the docked panels change, the panes when a result does", () => {
  for (const [from, to, docked, multiple, expected] of [
    ["#/", "#/settings", true, true, DOCKS],
    ["#/settings", "#/?panels=settings,endpoint", true, true, DOCKS],
    // The order of two docked panels decides which closes first, not where they stand.
    ["#/?panels=settings,endpoint", "#/?panels=endpoint,settings", true, true, null],
    // Where one panel fits, a route naming two shows its last: closing the hidden one moves nothing.
    ["#/?panels=settings,endpoint", "#/endpoint", true, false, null],
    ["#/settings", "#/endpoint", true, false, DOCKS],
    ["#/", "#/settings", false, false, null],
    ["#/settings", "#/history?panels=settings", true, true, null],
    ["#/history", `#/history/${ID}`, true, true, PANES],
    [`#/history/${ID}`, `#/history/${OTHER}`, true, true, PANES],
    [`#/history/${ID}`, "#/history", false, false, PANES],
    [`#/history/${ID}`, "#/", true, true, null],
    ["#/history", `#/history/${ID}?panels=settings`, true, true, DOCKS],
    ["#/not-found", "#/", true, true, null],
  ] as const)
    expect(moves(parseRoute(from), parseRoute(to), docked, multiple), `${from} → ${to}`).toEqual(expected);
});

test("focus goes to an opened panel's title, back from a closed panel it was in, and to a workspace reached by Back", () => {
  const facts = {
    holder: null,
    focused: () => true,
    wanted: null,
    fromHistory: false,
    flyout: false,
  } as const;
  for (const [from, to, shown, overrides, expected] of [
    ["#/", "#/settings", ["settings"], {}, { to: "title", panel: "settings" }],
    ["#/settings", "#/endpoint", ["details"], {}, { to: "title", panel: "details" }],
    ["#/endpoint", "#/?panels=endpoint&dialog=legal", ["details"], { wanted: "element" }, null],
    ["#/", "#/history", [], { wanted: "keep" }, null],
    ["#/", "#/history", [], { wanted: "element" }, { to: "wanted" }],
    ["#/history", "#/", [], { wanted: "workspace" }, { to: "workspace" }],
    ["#/settings", "#/", [], { holder: "settings" }, { to: "invoker", panel: "settings" }],
    // Focus elsewhere stays there; focus that fell to the page as the panel closed goes back to its invoker.
    ["#/settings", "#/", [], {}, null],
    ["#/settings", "#/", [], { focused: () => false }, { to: "invoker", panel: "settings" }],
    // Two docks collapsing into one close the first, which the layout already hid.
    [
      "#/?panels=settings,endpoint",
      "#/endpoint",
      ["details"],
      { holder: "settings", focused: () => false },
      { to: "invoker", panel: "settings" },
    ],
    ["#/?panels=settings,endpoint", "#/", [], { holder: "settings" }, { to: "invoker", panel: "settings" }],
    ["#/history", "#/", [], { fromHistory: true }, { to: "workspace" }],
    ["#/history?panels=settings", "#/?panels=settings", ["settings"], { fromHistory: true, flyout: true }, null],
    ["#/history", "#/?dialog=legal", [], { fromHistory: true }, null],
    ["#/history", "#/", [], {}, null],
  ] as const)
    expect(
      focusMove({ ...facts, ...overrides, before: parseRoute(from), after: parseRoute(to), shown }),
      `${from} → ${to}`,
    ).toEqual(expected);
});
