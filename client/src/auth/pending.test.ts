import { test, expect } from "bun:test";
import source from "./pending.js" with { type: "text" };

// pending.js is a digest-pinned classic script that exports nothing, so the test evaluates its bundled text.
type Landing = { redirected: boolean; url: string };
type Classifier = (response: Landing, here: { pathname: string }) => boolean;

const { leftThisPage } = new Function(
  "document",
  `${source}\nreturn { leftThisPage };`,
)({ addEventListener() {} }) as { leftThisPage: Classifier };

test("a rejection redirected back to the login page swaps in place", () => {
  const response = {
    redirected: true,
    url: "https://meter.example/login?error=invalid",
  };
  expect(leftThisPage(response, { pathname: "/login" })).toBe(false);
});

test("a sign-in redirected to the app navigates", () => {
  const response = { redirected: true, url: "https://meter.example/" };
  expect(leftThisPage(response, { pathname: "/login" })).toBe(true);
});

test("a sign-in redirected to the CLI approval page navigates", () => {
  const response = {
    redirected: true,
    url: "https://meter.example/auth/cli?challenge=abc",
  };
  expect(leftThisPage(response, { pathname: "/login" })).toBe(true);
});

test("an unredirected response swaps in place though its path differs", () => {
  const response = {
    redirected: false,
    url: "https://meter.example/auth/cli/approve",
  };
  expect(leftThisPage(response, { pathname: "/auth/cli" })).toBe(false);
});
