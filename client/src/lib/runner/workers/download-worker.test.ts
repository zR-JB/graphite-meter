import { expect, test } from "bun:test";
import {
  downloadFetchInit,
  recoverableDownloadStatus,
} from "./download-worker";

test("admission rejections are terminal for a download lane", () => {
  expect(recoverableDownloadStatus(429)).toBe(false);
  expect(recoverableDownloadStatus(503)).toBe(false);
  expect(recoverableDownloadStatus(500)).toBe(true);
});

test("download requests retain bearer credentials", () => {
  const controller = new AbortController();
  const init = downloadFetchInit(controller.signal, "include", {
    authorization: "Bearer grant",
  });
  expect(init.credentials).toBe("include");
  expect(init.redirect).toBe("error");
  expect(new Headers(init.headers).get("authorization")).toBe("Bearer grant");
});
