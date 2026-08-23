import { defineConfig, type Plugin } from "vite";
import { writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

// --- Build-time client configuration (see src/lib/buildenv.ts) -------------
// Driven by GM_CLIENT_* env vars (the justfile `prod` recipe / `docker build
// --build-arg` set them). Read from process.env here and injected via Vite
// `define` as RAW literal tokens: that literal substitution is what lets
// Rollup constant-fold + tree-shake the dummy runner and Developer tooling out
// of a production bundle. (`.env` / import.meta.env yields the *string*
// "false", which is truthy and defeats the tree-shaking.)
//
// Dev defaults (no env set): real engine, dummy + dev tools included, "dev"
// label.
const env = process.env;

const defaultEngine: "real" | "dummy" =
  env.GM_CLIENT_ENGINE === "dummy" ? "dummy" : "real";

// Boolean knobs default ON; only an explicit "0"/"false" turns them off.
const off = (v: string | undefined) => v === "0" || v === "false";
const allowDummy = !off(env.GM_CLIENT_ALLOW_DUMMY);
const devTools = !off(env.GM_CLIENT_DEV_TOOLS);

const buildProfile = env.GM_CLIENT_BUILD_PROFILE ?? "dev";
const releaseVersion = env.VERSION || null;
const sourceRevision =
  env.GM_CLIENT_REVISION ??
  (() => {
    try {
      return execFileSync("git", ["rev-parse", "--short", "HEAD"], {
        encoding: "utf8",
      }).trim();
    } catch {
      return "source";
    }
  })();
const clientVersion = releaseVersion ? `v${releaseVersion}` : sourceRevision;
const buildIdentity = `${buildProfile} ${clientVersion}`;

// Emit dist/version.json alongside the bundle (build only; dev serves no dist).
const versionFile = (): Plugin => ({
  name: "gm-version-file",
  generateBundle() {
    this.emitFile({
      type: "asset",
      fileName: "version.json",
      source: JSON.stringify({
        version: releaseVersion,
        label: buildProfile,
        revision: sourceRevision,
      }),
    });
  },
});

// The legal pipeline must describe code that is actually emitted, not every
// package installed for tests or build tooling. When requested, Rollup's
// emitted chunk module maps are written as a sorted, deterministic list. The
// scan build uses a temporary outDir, so it cannot disturb normal dist output.
const legalScan = (): Plugin => ({
  name: "gm-legal-scan",
  generateBundle(_options, bundle) {
    const output = env.GM_LEGAL_SCAN_OUT;
    if (!output) return;
    const modules = new Set<string>();
    for (const artifact of Object.values(bundle)) {
      if (artifact.type !== "chunk") continue;
      for (const moduleId of Object.keys(artifact.modules)) {
        modules.add(moduleId);
      }
    }
    writeFileSync(output, `${JSON.stringify([...modules].sort())}\n`, "utf8");
  },
});

// String.replace has no async callback support; needed since Bun.build is async.
async function replaceAsync(
  str: string,
  regex: RegExp,
  fn: (...match: string[]) => Promise<string>,
): Promise<string> {
  const matches: string[][] = [];
  str.replace(regex, (...args) => {
    matches.push(args.slice(0, -2) as string[]); // drop offset + full string
    return "";
  });
  const results = await Promise.all(matches.map((m) => fn(...m)));
  let i = 0;
  return str.replace(regex, () => results[i++]);
}

async function minifyInlineBlock(
  code: string,
  loader: "css" | "js",
): Promise<string> {
  const path = `/inline.${loader}`;
  const result = await Bun.build({
    entrypoints: [path],
    files: { [path]: code },
    minify: {
      whitespace: true,
      syntax: true,
      identifiers: loader === "js",
    },
  });
  if (!result.success || result.outputs.length === 0) {
    throw new Error(
      result.logs.map((log) => log.message).join("\n") ||
        `Bun failed to minify inline ${loader}`,
    );
  }
  return (await result.outputs[0].text()).trim();
}

// index.html isn't run through Rollup, so Vite never minifies it (comments,
// indentation, and the inline pre-paint <script>/<style> all ship as-authored).
// Runs post-injection (order: "post") so it also compacts the asset tags Vite
// writes in. Inline JS/CSS get real minification via Bun; the rest is
// comment stripping + collapsing whitespace between tags.
const minifyHtml = (): Plugin => ({
  name: "gm-minify-html",
  apply: "build",
  transformIndexHtml: {
    order: "post",
    handler: async (html) => {
      const withMinifiedBlocks = await replaceAsync(
        html,
        /<(script|style)\b([^>]*)>([\s\S]*?)<\/\1>/gi,
        async (_match, tag, attrs, inner) => {
          const isInlineScript =
            tag.toLowerCase() === "script" && !/\bsrc\s*=/i.test(attrs);
          const isStyle = tag.toLowerCase() === "style";
          const code =
            inner.trim() && (isInlineScript || isStyle)
              ? await minifyInlineBlock(inner, isStyle ? "css" : "js")
              : inner;
          return `<${tag}${attrs}>${code}</${tag}>`;
        },
      );

      // Placeholder swap keeps comment-stripping/whitespace-collapsing below
      // from reaching into the already-minified script/style bodies.
      const blocks: string[] = [];
      const withPlaceholders = withMinifiedBlocks.replace(
        /<(script|style)\b[^>]*>[\s\S]*?<\/\1>/gi,
        (block) => {
          blocks.push(block);
          return "\u0000" + (blocks.length - 1) + "\u0000";
        },
      );

      let withoutComments = withPlaceholders;
      let previous: string;
      do {
        previous = withoutComments;
        withoutComments = withoutComments.replace(/<!--[\s\S]*?-->/g, "");
      } while (withoutComments !== previous);

      const compacted = withoutComments
        .replace(/\s+/g, " ")
        .replace(/>\s+</g, "><")
        .trim();

      return compacted.replace(
        /\u0000(\d+)\u0000/g,
        (_, i) => blocks[Number(i)],
      );
    },
  },
});

export default defineConfig({
  plugins: [svelte(), tailwindcss(), versionFile(), minifyHtml(), legalScan()],
  build: {
    outDir: env.GM_LEGAL_SCAN_DIR ?? "dist",
  },
  define: {
    __GM_DEFAULT_ENGINE__: JSON.stringify(defaultEngine), // "real" | "dummy"
    __GM_ALLOW_DUMMY__: JSON.stringify(allowDummy), // bare true | false
    __GM_DEV_TOOLS__: JSON.stringify(devTools), // bare true | false
    __GM_BUILD_PROFILE__: JSON.stringify(buildProfile),
    __GM_RELEASE_VERSION__: JSON.stringify(releaseVersion),
    __GM_SOURCE_REVISION__: JSON.stringify(sourceRevision),
    __GM_CLIENT_VERSION__: JSON.stringify(clientVersion),
    __GM_BUILD_IDENTITY__: JSON.stringify(buildIdentity),
  },
});
