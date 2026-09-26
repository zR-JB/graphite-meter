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

test("hints change no cursor", async () => {
  expect(await offenders(/cursor:\s*help/)).toEqual([]);
});

test("hints draw no dotted underline", async () => {
  expect(
    await offenders(/underline\s+dotted|text-decoration-style:\s*dotted/),
  ).toEqual([]);
});

test("focus styles answer the keyboard only", async () => {
  expect(await offenders(/:focus(?![-\w])/)).toEqual([]);
});

test("only the disclosure primitive renders <details>", async () => {
  expect(
    await offenders(/<details\b/, ["lib/components/Disclosure.svelte"]),
  ).toEqual([]);
});
