import { expect, test } from "bun:test";
import { Glob } from "bun";

test("only Icon.svelte renders raw markup", async () => {
  const src = `${import.meta.dir}/..`;
  const sinks: string[] = [];
  for await (const file of new Glob("**/*.{svelte,ts}").scan(src))
    if (
      !file.endsWith(".test.ts") &&
      /\{@html\s|\.(inner|outer)HTML\b|insertAdjacentHTML|document\.write/.test(
        await Bun.file(`${src}/${file}`).text(),
      )
    )
      sinks.push(file);
  expect(sinks).toEqual(["lib/components/Icon.svelte"]);
});
