import { test, expect } from "bun:test";
import { decodeUploadProgress } from "./uploadProgress";

const fixtures: { name: string; record: unknown; valid: boolean }[] =
  await Bun.file(
    new URL(
      "../../../../../api/upload-progress.testvectors.json",
      import.meta.url,
    ),
  ).json();

for (const { name, record, valid } of fixtures) {
  test(`upload progress conformance: ${name}`, () => {
    const decoded = decodeUploadProgress(record);
    expect(decoded !== null).toBe(valid);
    if (decoded !== null) {
      expect(decodeUploadProgress(JSON.parse(JSON.stringify(decoded)))).toEqual(
        decoded,
      );
    }
  });
}
