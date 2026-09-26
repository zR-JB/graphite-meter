import { expect, test } from "bun:test";
import { Glob } from "bun";

test("only clock.ts reads a clock in the runner", async () => {
  const src = import.meta.dir;
  const readers: string[] = [];
  for await (const file of new Glob("**/*.ts").scan(src))
    if (
      !/^workers\/|\.(test|bench)\.ts$/.test(file) &&
      /performance\.now\(|Date\.now\(|new Date\(\)|Bun\.nanoseconds\(/.test(
        await Bun.file(`${src}/${file}`).text(),
      )
    )
      readers.push(file);
  expect(readers).toEqual(["clock.ts"]);
});
