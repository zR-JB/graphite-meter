import { expect, test } from "bun:test";
import { Glob } from "bun";

const SRC = `${import.meta.dir}/..`;

async function offenders(pattern: RegExp, allowed: string[] = []) {
  const found: string[] = [];
  for await (const file of new Glob("**/*.{svelte,css,ts}").scan(SRC)) {
    if (file.endsWith(".test.ts") || allowed.includes(file)) continue;
    const lines = (await Bun.file(`${SRC}/${file}`).text()).split("\n");
    lines.forEach((line, index) => {
      if (pattern.test(line)) found.push(`${file}:${index + 1}`);
    });
  }
  return found;
}

test("only the shared explained-word rule takes the help cursor", async () => {
  expect(await offenders(/cursor:\s*help/, ["app.css"])).toEqual([]);
});

test("only the shared term mark draws a dotted underline", async () => {
  expect(
    await offenders(/underline\s+dotted|text-decoration-style:\s*dotted/, [
      "app.css",
    ]),
  ).toEqual([]);
});

test("focus styles answer the keyboard only", async () => {
  expect(await offenders(/:focus(?![-\w])/)).toEqual([]);
});

test("facts sit in grouped lists, never behind <details>", async () => {
  expect(await offenders(/<details\b/)).toEqual([]);
});
