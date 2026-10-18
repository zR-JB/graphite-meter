// Makes the auth pages' embedded assets and public/favicon.svg; `--check` fails when committed files differ.
import { readdir } from "node:fs/promises";
import { join, relative } from "node:path";
import { BRAND_LINE, favicon } from "./brand";

const here = import.meta.dir;
const output = join(here, "../../rust/server/src/auth/assets");
// The only faces the server serves before sign-in (rust/server/src/auth/policy.rs).
const FACES = ["/fonts/ibm-plex-sans-var-latin1.woff2", "/fonts/ibm-plex-mono-600-latin1.woff2"];
const STYLES = join(here, "../src/styles");
// The app's rules the pages use, by selector, in cascade order: unlayered there, the tones beat .notice.
const SHARED = [
  "*, ::before, ::after, ::backdrop",
  '[hidden]:not([hidden="until-found"])',
  ".notice",
  ".sr-only",
  "[data-tone]",
  '[data-tone="neutral"]',
  '[data-tone="warn"]',
  '[data-tone="err"]',
];

/** `code` built minified as the file `path` would be, with what it imports. */
export async function minified(path: string, code: string, options: Partial<Bun.BuildConfig> = {}): Promise<string> {
  const result = await Bun.build({ entrypoints: [path], files: { [path]: code }, minify: true, ...options });
  return (await result.outputs[0]!.text()).trim();
}

// Every browser the pages support has :is(); the prefixed copies Bun adds beside it would outrank it in Chrome.
export const withoutPrefixedIs = (css: string) =>
  css.replace(/(?<=^|[{}])[^{}@]*:-(?:webkit|moz)-any\([^{}]*\{[^{}]*\}/g, "");

/** `source`'s module graph as one dependency-free classic script, for a server or the shell to inline and pin. */
const classicScript = (source: string) => minified(join(here, "entry.ts"), source, { format: "iife" });

export const themeScript = () => classicScript(`import { prePaintTheme } from "./theme"; prePaintTheme();`);

type Item = string | { prelude: string; body: Item[] };

/** Statements, declarations and blocks, without comments: as much CSS as the app's stylesheets are written in. */
function parse(css: string): Item[] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "");
  let at = 0;
  const items = (): Item[] => {
    const list: Item[] = [];
    let start = at;
    for (; at < text.length; at++) {
      const char = text[at];
      if (char === '"' || char === "'") {
        at = text.indexOf(char, at + 1);
        if (at < 0) throw new Error("an app stylesheet has an unterminated string");
      } else if (char === "{") {
        const prelude = text.slice(start, at).trim();
        at++;
        list.push({ prelude, body: items() });
        start = at + 1;
      } else if (char === ";" || char === "}") {
        const statement = text.slice(start, at).trim();
        if (statement) list.push(statement);
        start = at + 1;
        if (char === "}") return list;
      }
    }
    return list;
  };
  return items();
}

/** The faces the pages may load, the `:root` tokens their rules use, transitively, with each override's media, the
    SHARED rules and then auth.css's own. */
async function stylesheet(): Promise<{ css: string; tokens: Map<string, string> }> {
  const own = (await Bun.file(join(here, "auth.css")).text()).replace(/\/\*[\s\S]*?\*\//g, "");
  const faces: string[][] = [];
  type Rule = { media: string | null; selector: string; declarations: [string, string][] };
  const roots: Rule[] = [];
  const shared: Rule[] = [];
  const walk = (items: Item[], media: string | null, tokens: boolean) => {
    for (const item of items) {
      if (typeof item === "string") continue;
      const lines = item.body.filter((line) => typeof line === "string");
      const selector = item.prelude.replace(/\s+/g, " ");
      const rule = () => ({
        media,
        selector,
        declarations: lines.map((line): [string, string] => {
          const [name, value] = line.split(/:(.*)/s);
          return [name!.trim(), value!.trim().replace(/\s+/g, " ")];
        }),
      });
      if (item.prelude === "@font-face") {
        if (tokens && FACES.some((face) => lines.some((line) => line.includes(`url("${face}")`)))) faces.push(lines);
      } else if (item.prelude.startsWith("@layer")) walk(item.body, media, tokens);
      else if (item.prelude.startsWith("@media")) walk(item.body, item.prelude, tokens);
      else if (tokens && item.prelude.startsWith(":root")) roots.push(rule());
      else if (SHARED.includes(selector)) shared.push(rule());
    }
  };
  for (const name of ["tokens.css", "base.css", "primitives.css", "overrides.css"])
    walk(parse(await Bun.file(join(STYLES, name)).text()), null, name === "tokens.css");
  if (faces.length !== FACES.length)
    throw new Error(`tokens.css must declare one face for each of ${FACES.join(", ")}`);
  const missing = SHARED.filter((selector) => shared.filter((rule) => rule.selector === selector).length !== 1);
  if (missing.length) throw new Error(`src/styles must declare each of ${missing.join(", ")} once`);

  const block = (head: string, lines: string[]) => `${head}{${lines.join("")}}`;
  const print = ({ media, selector, declarations }: Rule) => {
    const rule = block(
      selector,
      declarations.map(([name, value]) => `${name}:${value};`),
    );
    return media ? block(media, [rule]) : rule;
  };
  const rules = shared
    .sort((a, b) => SHARED.indexOf(a.selector) - SHARED.indexOf(b.selector))
    .map(print)
    .concat(own)
    .join("\n");

  const declared = Map.groupBy(
    roots.flatMap((rule) => rule.declarations),
    ([name]) => name,
  );
  const local = new Set(Array.from(rules.matchAll(/(--[\w-]+)\s*:/g), (match) => match[1]));
  const used = new Set<string>();
  const use = (text: string) => {
    for (const [, name] of text.matchAll(/var\(\s*(--[\w-]+)/g)) {
      if (used.has(name!) || (!declared.has(name!) && local.has(name!))) continue;
      if (!declared.has(name!)) throw new Error(`auth.css uses ${name}, which src/styles/tokens.css does not declare`);
      used.add(name!);
      for (const [, value] of declared.get(name!)!) use(value);
    }
  };
  use(rules);

  const css = [
    ...faces.map((face) =>
      block(
        "@font-face",
        face.map((line) => `${line};`),
      ),
    ),
    ...roots.flatMap((rule) => {
      const kept = rule.declarations.filter(([name]) => !name.startsWith("--") || used.has(name));
      return kept.length ? [print({ ...rule, declarations: kept })] : [];
    }),
    rules,
  ];
  const tokens = new Map(roots.filter((rule) => !rule.media).flatMap((rule) => rule.declarations));
  const minifiedCss = await minified(join(here, "entry.css"), css.join("\n"), { external: ["/fonts/*"] });
  return { css: withoutPrefixedIs(minifiedCss), tokens };
}

/** Every generated file by path. */
async function generated(): Promise<Map<string, string>> {
  const { css, tokens } = await stylesheet();
  const svg = favicon(tokens.get("--phase-latency")!, tokens.get("--text")!);
  const assets: Record<string, string> = {
    "auth.css": css,
    "theme.js": await themeScript(),
    "pending.js": await classicScript(`import { install } from "./pending"; install(window);`),
    "README.md":
      "client/auth/build.ts makes these files and client/public/favicon.svg: edit its sources and run `bun run build:auth` in client.\n",
    "icon.html": `<link rel="icon" href="data:image/svg+xml;base64,${btoa(svg)}">`,
    "brand.html": BRAND_LINE,
  };
  const files = new Map(Object.entries(assets).map(([name, text]) => [join(output, name), text]));
  return files.set(join(here, "../public/favicon.svg"), svg);
}

if (import.meta.main) {
  const files = await generated();
  const stale = Array.from(await readdir(output), (name) => join(output, name)).filter((path) => !files.has(path));
  for (const [path, wanted] of files) {
    const file = Bun.file(path);
    // Bytes, not text: a decoder would hide a byte-order mark the server embeds.
    if (!(await file.exists()) || !Buffer.from(wanted).equals(await file.bytes())) stale.push(path);
  }
  if (process.argv.includes("--check")) {
    if (stale.length) {
      const paths = stale.map((path) => relative(join(here, "../.."), path)).sort();
      console.error(`client/auth makes different files: ${paths.join(", ")}.`);
      console.error("Run `bun run build:auth` in client, and remove any file it does not make.");
      process.exit(1);
    }
  } else for (const path of stale) if (files.has(path)) await Bun.write(path, files.get(path)!);
}
