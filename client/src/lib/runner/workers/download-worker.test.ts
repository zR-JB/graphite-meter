import { expect, test } from "bun:test";
import {
  downloadFetchInit,
  recoverableDownloadStatus,
} from "./download-worker";
import { authenticationRequired } from "../../request-auth";

test("admission rejections are terminal for a download lane", () => {
  expect(recoverableDownloadStatus(429)).toBe(false);
  expect(recoverableDownloadStatus(503)).toBe(false);
  expect(recoverableDownloadStatus(500)).toBe(true);
});

test("download requests retain bearer credentials", () => {
  const init = downloadFetchInit("include", {
    authorization: "Bearer grant",
  });
  expect(init.credentials).toBe("include");
  expect(init.redirect).toBe("error");
  expect(new Headers(init.headers).get("authorization")).toBe("Bearer grant");
});

// Only the marker means an expired session. A bare 403 from a proxy in front
// of the server means a transfer failure. Treating that as `auth-required`
// tears the run down and navigates away from the console.
test("only a marker-qualified 403 stops a lane as auth-required", () => {
  const response = (status: number, headers: Record<string, string> = {}) =>
    new Response("", { status, headers });

  expect(
    authenticationRequired(
      response(403, { "Graphite-Meter-Auth": "required" }),
    ),
  ).toBe(true);
  expect(authenticationRequired(response(403))).toBe(false);
  expect(
    authenticationRequired(response(403, { "Graphite-Meter-Auth": "denied" })),
  ).toBe(false);

  // An unqualified 403 stays on the ordinary error path, where the lane restarts.
  expect(recoverableDownloadStatus(403)).toBe(true);
});
