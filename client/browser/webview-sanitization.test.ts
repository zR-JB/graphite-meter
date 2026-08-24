import { expect, test } from "./webview";

test("locator data remains data when embedded in WebView evaluation", async ({
  page,
}) => {
  await page.goto("/?engine=dummy");
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
