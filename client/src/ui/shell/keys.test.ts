import { expect, test } from "bun:test";
import { command, type KeyFacts } from "./keys";

const idle: KeyFacts = {
  key: "Escape",
  editable: false,
  ownsSpace: false,
  shortcuts: true,
  saving: true,
  workspace: "measurement",
  detail: false,
  panels: [],
  focused: null,
  flyout: false,
  pending: false,
  running: false,
  modified: false,
};

test("Escape closes the topmost surface it reaches, and stops a run only when none is left", () => {
  for (const [facts, expected] of [
    [
      { panels: ["settings", "details"], focused: "settings" },
      { kind: "close", panel: "settings" },
    ],
    [{ panels: ["settings", "details"] }, { kind: "close", panel: "details" }],
    // A flyout closes even from a text field. A docked panel closes only from a field inside it.
    [
      { panels: ["settings"], flyout: true, editable: true },
      { kind: "close", panel: "settings" },
    ],
    [{ panels: ["settings"], editable: true }, null],
    [
      { panels: ["details"], focused: "details", editable: true },
      { kind: "close", panel: "details" },
    ],
    [{ workspace: "history", detail: true, panels: ["settings"] }, { kind: "close-detail" }],
    [{ workspace: "history" }, { kind: "close-history" }],
    [
      { workspace: "history", panels: ["settings"], flyout: true },
      { kind: "close", panel: "settings" },
    ],
    [{ workspace: "not-found", panels: ["settings"] }, null],
    [{ pending: true }, { kind: "cancel-start" }],
    [
      { panels: ["details"], pending: true },
      { kind: "close", panel: "details" },
    ],
    [
      { shortcuts: false, panels: ["details"] },
      { kind: "close", panel: "details" },
    ],
    [{ running: true }, { kind: "stop" }],
    [
      { panels: ["settings"], running: true },
      { kind: "close", panel: "settings" },
    ],
    [{ running: true, editable: true }, null],
    [{ running: true, shortcuts: false }, null],
    [{ running: true, modified: true }, null],
    [{ workspace: "history", running: true }, { kind: "close-history" }],
    [{}, null],
  ] as const)
    expect(command({ ...idle, ...facts })).toEqual(expected);
});

test("shortcut keys act where they apply; text entry keeps its keys", () => {
  for (const [facts, expected] of [
    [{ key: " " }, { kind: "run" }],
    [{ key: " ", ownsSpace: true }, null],
    [{ key: "r", ownsSpace: true }, { kind: "run" }],
    [{ key: "R" }, { kind: "run" }],
    [{ key: " ", workspace: "history" }, null],
    [
      { key: "s", workspace: "history" },
      { kind: "toggle", panel: "settings" },
    ],
    [{ key: "d" }, { kind: "toggle", panel: "details" }],
    [{ key: "h" }, { kind: "history" }],
    [{ key: "h", saving: false }, null],
    [{ key: "t", workspace: "not-found" }, { kind: "theme" }],
    [{ key: "s", editable: true }, null],
    [{ key: "s", shortcuts: false }, null],
    [{ key: " ", shortcuts: false }, null],
    [{ key: "x" }, null],
  ] as const)
    expect(command({ ...idle, ...facts })).toEqual(expected);
});
