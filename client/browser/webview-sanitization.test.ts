import { expect, openApp, test } from "./webview";
test("unsupported route stubs fail instead of ignoring the callback", async ({
  page,
}) => {
  await expect(
    page.route("https://example.invalid/**", () => {}),
  ).rejects.toThrow("local server paths only");
});

test("network blocking prevents a request from reaching the server", async ({
  page,
}) => {
  let requests = 0;
  await page.route("**/blocked-probe", (route) => {
    requests++;
    return route.fulfill({ body: "reachable" });
  });
  await openApp(page);
  expect(
    await page.evaluate(() =>
      fetch("/blocked-probe").then((response) => response.text()),
    ),
  ).toBe("reachable");
  await page.blockRequests("*/blocked-probe");
  expect(
    await page.evaluate(() =>
      fetch("/blocked-probe").then(
        () => "unexpected response",
        () => "blocked",
      ),
    ),
  ).toBe("blocked");
  expect(requests).toBe(1);
});

test("locator data remains data when embedded in WebView evaluation", async ({
  page,
}) => {
  await openApp(page);
  const hostile = `</script><script>globalThis.__gmInjected = true</script>`;
  const payload = `${hostile}\u2028\u2029`;
  await page.evaluate(
    ([text, data]) => {
      const button = document.createElement("button");
      button.textContent = text;
      button.dataset.payload = data;
      document.body.append(button);
    },
    [hostile, payload],
  );
  const button = page.getByRole("button", { name: hostile, exact: true });
  await expect(button).toHaveCount(1);
  expect(await button.getAttribute("data-payload")).toBe(payload);
  expect(
    await page.evaluate(
      () =>
        (globalThis as typeof globalThis & { __gmInjected?: boolean })
          .__gmInjected,
    ),
  ).toBeUndefined();
});

test("hover waits for an entering target to stop moving", async ({ page }) => {
  await openApp(page);
  await page.evaluate(() => {
    const target = document.createElement("button");
    target.id = "moving-hover-target";
    target.textContent = "Moving hover target";
    target.style.cssText =
      "position:fixed;left:200px;top:200px;width:160px;height:24px;z-index:9999";
    document.body.append(target);
    target.addEventListener("pointerenter", () => {
      target.dataset.enteredWhileMoving = String(
        target
          .getAnimations()
          .some((animation) => animation.playState === "running"),
      );
    });
    target.animate(
      [{ transform: "translateY(100px)" }, { transform: "translateY(0)" }],
      { duration: 600, fill: "both" },
    );
  });
  const target = page.locator("#moving-hover-target");
  await target.hover();
  expect(await target.getAttribute("data-entered-while-moving")).toBe("false");
  expect(await target.evaluate((node) => node.matches(":hover"))).toBe(true);
});
