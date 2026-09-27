import { expect, test } from "bun:test";
import { FAILURE_REASONS } from "../runner/contract";
import { reasonLabel } from "./vocabulary";

test("failure labels match the shared pin", async () => {
  const pin = await Bun.file(
    `${import.meta.dir}/../../../../api/failurereasons.txt`,
  ).text();
  const pinned = pin
    .split("\n")
    .filter((line) => line.trim() && !line.startsWith("#"))
    .map((line) => line.split("|").map((cell) => cell.trim()));
  expect(FAILURE_REASONS.map((r) => [r, reasonLabel(r)])).toEqual(pinned);
});
