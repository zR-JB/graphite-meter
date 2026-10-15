import { expect, test } from "bun:test";
import { json } from "./pins.testutil";
import { progressRecord } from "./progress";

test("the shared upload progress records", async () => {
  for (const { name, record, valid } of await json("upload-progress.testvectors.json")) {
    const decode = () => progressRecord(record);
    if (valid) expect(decode, name).not.toThrow();
    else expect(decode, name).toThrow();
  }
});
