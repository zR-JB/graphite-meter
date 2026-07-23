import { test, expect } from "bun:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

// Svelte's {@html expr} bypasses auto-escaping, so any server- or user-derived
// string reaching one is stored/reflected XSS. The application ships with only
// a permissive script-src on its own pages (the login surface is hash-pinned),
// so an XSS there reads the CSRF token, mints a measurement grant, and forges
// same-origin requests — a full account-scope compromise. This test is the
// guard: every {@html} must render a vetted static-icon expression and nothing
// else. Adding an entry certifies you checked the expression can only ever hold
// trusted, build-time SVG markup — never a value that crossed the network.
const ALLOWED = new Set([
  "ICON.bidirectional",
  "ICON.bolt",
  "ICON.check",
  "ICON.close",
  "ICON.info",
  "ICON.settings",
  "THEME_ICON[store.theme]",
  // Local loop variables that only ever hold ICON.* values:
  //   StageTrack: s.icon from a static STAGES table
  //   ResultCards: c.icon assigned ICON.download/upload/bidirectional/ping
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
      `XSS sink — render as text or sanitize:\n${offenders.join("\n")}`,
  ).toEqual([]);
});
