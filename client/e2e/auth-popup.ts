import { fleet, test, expect } from "./multi-server-fixtures";
import { openSettings } from "../browser/webview";
import { configure } from "./multi-server-actions";

test("peer sign-in automatically opens an isolated popup during the user click", async ({
  page,
}) => {
  await page.addInitScript(() => {
    const open = window.open.bind(window);
    const state = { active: false, isolated: false, calls: 0 };
    Object.assign(window, { approvalPopupEvidence: state });
    window.open = (...args) => {
      state.active = navigator.userActivation.isActive;
      state.calls++;
      const popup = open(...args);
      queueMicrotask(() => {
        state.isolated = popup !== null && popup.opener === null;
      });
      return popup;
    };
  });
  await configure(page, ["self", fleet[4].id]);
  await openSettings(page);
  const row = page.locator(".server-feedback", { hasText: "Private" });
  await expect(
    row.getByRole("button", { name: "Sign in to Private" }),
  ).toBeVisible({ timeout: 15000 });
  const point = await row
    .getByRole("button", { name: "Sign in to Private" })
    .evaluate((button) => {
      const rect = button.getBoundingClientRect();
      return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
    });
  for (const type of ["mousePressed", "mouseReleased"])
    await page.raw.cdp("Input.dispatchMouseEvent", {
      type,
      ...point,
      button: "left",
      clickCount: 1,
    });
  const link = row.getByRole("link", { name: "Open sign-in page" });
  await expect(link).toBeVisible();
  const approvalURL = new URL((await link.getAttribute("href"))!);
  const targets = async () => {
    const result = (await page.raw.cdp("Target.getTargets")) as {
      targetInfos: { targetId: string; url: string }[];
    };
    return result.targetInfos.filter(
      ({ url }) =>
        url.startsWith(fleet[4].url) &&
        url.includes(approvalURL.searchParams.get("challenge")!),
    );
  };
  try {
    await expect.poll(async () => (await targets()).length).toBe(1);
    expect(
      await page.evaluate(
        () =>
          (window as unknown as { approvalPopupEvidence: unknown })
            .approvalPopupEvidence,
      ),
    ).toEqual({ active: true, isolated: true, calls: 1 });
    await row.getByRole("button", { name: "Cancel sign-in" }).click();
    await expect(link).toHaveCount(0);
  } finally {
    // COOP may detach the popup handle, so cleanup must not rely on window.close.
    for (const target of await targets())
      await page.raw.cdp("Target.closeTarget", { targetId: target.targetId });
  }
});
