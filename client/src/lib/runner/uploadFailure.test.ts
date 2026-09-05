import { expect, test } from "bun:test";
import { classifyUploadFailure } from "./uploadFailure";
test("upload refusals retain their explicit protocol cause", () => {
  expect(classifyUploadFailure(400, "invalid")).toBe("unknown-upload-id");
  for (const [status, code, cause] of [
    [403, "ownerMismatch", "owner-mismatch"],
    [429, "clientFull", "capacity-refusal"],
    [503, "globalFull", "capacity-refusal"],
    [401, null, "authentication-failure"],
    [400, null, "protocol-refusal"],
  ] as const) {
    const classified = classifyUploadFailure(status, code);
    expect(classified).toBe(cause);
  }
});
