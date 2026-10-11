// The shell's contract at the fleet's home server, through the browser harness v2 shares with v1.
import type { Server } from "../../client/e2e/servers";
import { expect, test } from "../../client/e2e/webview";

const fleet = process.env.GM_E2E;
if (!fleet) throw new Error("GM_E2E is missing; run the suite with mise run e2e-v2");
const home: Server = JSON.parse(fleet).fleet[0];

// The harness fails a test on any console error, policy violations included.
test("the shell loads under the server's policy and reports a production build", async (page) => {
  await page.goto(home.url);
  await expect(page.locator("#console")).toHaveCount(1);
  const version = await fetch(`${home.http}/version.json`);
  expect((await version.json()).label).toBe("prod");
});
