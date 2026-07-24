import { test, expect } from "bun:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

// Svelte's {@html expr} bypasses auto-escaping. A server- or user-derived
// string reaching one is XSS. App pages carry a permissive script-src; only
// the login surface is hash-pinned. That XSS reads the CSRF token, mints a
// measurement grant, and forges same-origin requests: account compromise.

// Expressions vetted as build-time SVG markup. An entry asserts the value never
// carries anything from the network.
const ALLOWED = new Set([
  "ICON.bidirectional",
  "ICON.bolt",
  "ICON.check",
  "ICON.close",
  "ICON.info",
  "ICON.settings",
  "THEME_ICON[store.theme]",
  // Loop variables holding ICON.* only: StageTrack s.icon from the static
  // STAGES table, ResultCards c.icon from its static card table.
  "s.icon",
  "c.icon",
]);

function svelteFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) out.push(...svelteFiles(full));
    else if (entry.endsWith(".svelte")) out.push(full);
  }
  return out;
}

test("every {@html} sink renders only a vetted static-icon expression", () => {
  const offenders: string[] = [];
  for (const file of svelteFiles(join(import.meta.dir, "components"))) {
    const source = readFileSync(file, "utf8");
    for (const match of source.matchAll(/\{@html\s+([^}]+)\}/g)) {
      const expr = match[1].trim();
      if (!ALLOWED.has(expr)) offenders.push(`${file}: {@html ${expr}}`);
    }
  }
  expect(
    offenders,
    `Unvetted {@html} sink(s). If the expression can only hold trusted ` +
      `build-time markup, add it to ALLOWED in this file; otherwise it is an ` +
      `XSS sink: render as text or sanitize.\n${offenders.join("\n")}`,
  ).toEqual([]);
});
