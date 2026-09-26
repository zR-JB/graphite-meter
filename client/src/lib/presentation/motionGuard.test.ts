import { expect, test } from "bun:test";
import { Glob } from "bun";

const ROOT = `${import.meta.dir}/../`;
const OWNER = "presentation/motion.svelte.ts";
const CLOCK =
  /requestAnimationFrame|performance\.now\(|Date\.now\(|\.animate\(|prefersReducedMotion/;
const TIMER = /\bset(Timeout|Interval)\(/;

test("only the motion module reads the clock, runs frames or reads reduced motion; timers say why they are not motion", async () => {
  const problems: string[] = [];
  let scanned = 0;
  const glob = new Glob(
    "{state,components,presentation,canvas,actions}/**/*.{ts,svelte}",
  );
  for await (const path of glob.scan(ROOT)) {
    if (path === OWNER || /\.test\.ts$|testutil/.test(path)) continue;
    const lines = (await Bun.file(ROOT + path).text()).split("\n");
    scanned++;
    lines.forEach((line, index) => {
      const reason = /^\s*\/\/ Not motion: /.test(lines[index - 1] ?? "");
      if (CLOCK.test(line) || (TIMER.test(line) && !reason))
        problems.push(`${path}:${index + 1}: ${line.trim()}`);
    });
  }
  expect(scanned).toBeGreaterThan(50);
  expect(problems).toEqual([]);
});
