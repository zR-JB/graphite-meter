import { test, expect } from "./webview";

test("native popovers stay reachable after their scroll anchor leaves a short viewport", async ({
  page,
}) => {
  const { createServer } = await import("vite");
  const server = await createServer({
    server: { host: "127.0.0.1", port: 0 },
    logLevel: "error",
  });
  try {
    await server.listen();
    await page.setViewportSize({ width: 568, height: 320 });
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.goto(server.resolvedUrls!.local[0]);
    await page.evaluate(async () => {
      const runtimePath = "/node_modules/.vite/deps/svelte.js";
      const selectorPath = "/src/lib/components/ServerSelector.svelte";
      const detailsPath = "/src/lib/components/DiagnosticDetails.svelte";
      const { mount, createRawSnippet } = await import(runtimePath);
      const [{ default: Selector }, { default: Details }] = await Promise.all([
        import(selectorPath),
        import(detailsPath),
      ]);
      const scroll = document.createElement("div");
      scroll.id = "popover-scroll";
      scroll.style.cssText =
        "position:fixed;inset:0;overflow:auto;z-index:999;background:var(--surface-1);padding:16px";
      const before = document.createElement("div");
      before.style.height = "800px";
      const target = document.createElement("div");
      const after = document.createElement("div");
      after.style.height = "800px";
      scroll.append(before, target, after);
      document.body.append(scroll);
      mount(Selector, {
        target,
        props: {
          servers: [
            { id: "one", name: "First server", url: "https://one.example" },
            { id: "two", name: "Second server", url: "https://two.example" },
          ],
          value: "one",
          label: "Viewport selector",
          onchange: () => {},
        },
      });
      mount(Details, {
        target,
        props: {
          label: "Viewport details",
          children: createRawSnippet(() => ({
            render: () =>
              `<div>${"<p>Saved diagnostic outcome</p>".repeat(20)}</div>`,
          })),
        },
      });
      scroll.scrollTop = 680;
    });
    const cdp = await page.context.newCDPSession(page);
    for (const role of ["combobox", "button"] as const) {
      await page.locator("#popover-scroll").evaluate((node) => {
        node.scrollTop = 680;
      });
      const trigger = page.getByRole(role, {
        name: role === "combobox" ? "Viewport selector" : "Viewport details",
        exact: true,
      });
      await trigger.click();
      const popup = page.getByRole(role === "combobox" ? "listbox" : "dialog", {
        name: role === "combobox" ? "Viewport selector" : "Viewport details",
        exact: true,
      });
      await expect(popup).toBeVisible();
      await cdp.send("Emulation.setPageScaleFactor", { pageScaleFactor: 2 });
      await expect
        .poll(() =>
          popup.evaluate((node) => {
            const box = node.getBoundingClientRect();
            const viewport = visualViewport!;
            return (
              box.left >= viewport.offsetLeft &&
              box.top >= viewport.offsetTop &&
              box.right <= viewport.offsetLeft + viewport.width &&
              box.bottom <= viewport.offsetTop + viewport.height
            );
          }),
        )
        .toBe(true);
      await cdp.send("Emulation.setPageScaleFactor", { pageScaleFactor: 1 });
      await page.locator("#popover-scroll").evaluate((node) => {
        node.scrollTop = 0;
      });
      await expect
        .poll(() =>
          popup.evaluate((node) => {
            if (!node.matches(":popover-open")) return true;
            const box = node.getBoundingClientRect();
            return box.top >= 0 && box.bottom <= innerHeight;
          }),
        )
        .toBe(true);
      await page.evaluate(() =>
        document
          .querySelectorAll<HTMLElement>("[popover]:popover-open")
          .forEach((node) => node.hidePopover()),
      );
    }
  } finally {
    await server.close();
  }
});
