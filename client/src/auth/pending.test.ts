import { test, expect } from "bun:test";
import source from "../../../go/internal/auth/assets/pending.js" with { type: "text" };

// pending.js is a digest-pinned classic script that exports nothing, so the test evaluates its bundled text.
type Landing = { redirected: boolean; url: string };
type Classifier = (response: Landing, here: { pathname: string }) => boolean;

const { leftThisPage } = new Function(
  "document",
  `${source}\nreturn { leftThisPage };`,
)({ addEventListener() {}, querySelectorAll: () => [] }) as {
  leftThisPage: Classifier;
};

test("only a redirect to another page leaves this page", () => {
  for (const [redirected, url, pathname, left] of [
    [true, "https://meter.example/login?error=invalid", "/login", false],
    [true, "https://meter.example/", "/login", true],
    [true, "https://meter.example/auth/cli?challenge=abc", "/login", true],
    [false, "https://meter.example/auth/cli/approve", "/auth/cli", false],
  ] as const)
    expect(leftThisPage({ redirected, url }, { pathname })).toBe(left);
});
