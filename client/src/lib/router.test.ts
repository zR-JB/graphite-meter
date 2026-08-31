import { describe, expect, test } from "bun:test";
import {
  activatePanel,
  closeDialog,
  closePanel,
  openDialog,
  parseRoute,
  reconcilePanels,
  serializeRoute,
  withWorkspace,
} from "./router";

describe("hash router", () => {
  test("round trips friendly routes and composed history surfaces", () => {
    expect(serializeRoute(parseRoute("#/history"))).toBe("#/history");
    expect(serializeRoute(parseRoute("#/settings"))).toBe("#/settings");
    expect(
      serializeRoute(
        parseRoute("#/history?panels=settings,endpoint&dialog=legal"),
      ),
    ).toBe("#/history?panels=settings,endpoint&dialog=legal");
  });

  test("friendly routes retain composed panels and dialogs", () => {
    expect(serializeRoute(parseRoute("#/settings?dialog=legal"))).toBe(
      "#/?panels=settings&dialog=legal",
    );
    expect(serializeRoute(parseRoute("#/endpoint?panels=settings"))).toBe(
      "#/?panels=endpoint,settings",
    );
    expect(
      serializeRoute(
        parseRoute(
          "#/history/00000000-0000-4000-8000-000000000127?panels=settings,settings&dialog=legal",
        ),
      ),
    ).toBe(
      "#/history/00000000-0000-4000-8000-000000000127?panels=settings&dialog=legal",
    );
  });

  test("keeps the base workspace while panels and Legal open and close", () => {
    const history = parseRoute(
      "#/history/00000000-0000-4000-8000-000000000127",
    );
    const settings = activatePanel(history, "settings");
    const endpoint = activatePanel(settings, "endpoint");
    expect(endpoint).toMatchObject({
      kind: "app",
      workspace: {
        kind: "history",
        selectedId: "00000000-0000-4000-8000-000000000127",
      },
      panels: ["settings", "endpoint"],
    });
    expect(closePanel(openDialog(endpoint, "legal"), "settings")).toMatchObject(
      {
        workspace: { kind: "history" },
        panels: ["endpoint"],
        dialog: "legal",
      },
    );
    expect(closeDialog(openDialog(endpoint, "legal"))).toEqual(endpoint);
  });

  test("retains ordered panels on wide layouts but only the visible panel on narrow layouts", () => {
    const route = parseRoute("#/?panels=endpoint,settings");
    expect(activatePanel(route, "endpoint").panels).toEqual([
      "settings",
      "endpoint",
    ]);
    expect(activatePanel(route, "settings", false).panels).toEqual([
      "settings",
    ]);
    expect(reconcilePanels(route, false)).toEqual({
      kind: "app",
      workspace: { kind: "measurement" },
      panels: ["settings"],
      dialog: null,
    });
    expect(reconcilePanels(route, true)).toEqual(route);
  });

  test("switches workspaces without changing requested auxiliary surfaces", () => {
    const route = parseRoute("#/history?panels=settings&dialog=legal");
    expect(withWorkspace(route, { kind: "measurement" })).toEqual({
      kind: "app",
      workspace: { kind: "measurement" },
      panels: ["settings"],
      dialog: "legal",
    });
  });

  test("unknown and invalid routes become a client not-found state", () => {
    expect(parseRoute("#/unknown")).toEqual({ kind: "not-found" });
    expect(parseRoute("#/history/not-a-uuid")).toEqual({ kind: "not-found" });
    expect(parseRoute("#/history/%zz")).toEqual({ kind: "not-found" });
  });
});
