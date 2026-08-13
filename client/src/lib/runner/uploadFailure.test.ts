import { expect, test } from "bun:test";
import { classifyUploadFailure, uploadFailureMayRotate } from "./uploadFailure";

test("only an explicit invalid upload id may rotate the session", () => {
  expect(classifyUploadFailure(400, "invalid")).toBe("unknown-upload-id");
  expect(uploadFailureMayRotate("unknown-upload-id")).toBe(true);

  for (const [status, code, cause] of [
    [403, "ownerMismatch", "owner-mismatch"],
    [429, "clientFull", "capacity-refusal"],
    [503, "globalFull", "capacity-refusal"],
    [401, null, "authentication-failure"],
    [400, null, "protocol-refusal"],
  ] as const) {
    const classified = classifyUploadFailure(status, code);
    expect(classified).toBe(cause);
    expect(uploadFailureMayRotate(classified)).toBe(false);
  }
});
