import { defineConfig, type Plugin } from "vite";
import { writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

// Build-time GM_CLIENT_* literals enable tree-shaking for optional browser fixtures.
const env = process.env;

// Boolean knobs default ON; only an explicit "0"/"false" turns them off.
const off = (v: string | undefined) => v === "0" || v === "false";
const allowDummy = !off(env.GM_CLIENT_ALLOW_DUMMY);

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

// The legal scan records sorted emitted-chunk modules in a temporary outDir, excluding test/build-only dependencies.
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

// Post-injection minification compacts index.html and Vite asset tags while preserving inline block bodies.
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

      // Placeholders keep comment stripping and whitespace collapsing out of minified script/style bodies.
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
    __GM_ALLOW_DUMMY__: JSON.stringify(allowDummy),
    __GM_BUILD_PROFILE__: JSON.stringify(buildProfile),
    __GM_RELEASE_VERSION__: JSON.stringify(releaseVersion),
    __GM_SOURCE_REVISION__: JSON.stringify(sourceRevision),
    __GM_CLIENT_VERSION__: JSON.stringify(clientVersion),
    __GM_BUILD_IDENTITY__: JSON.stringify(buildIdentity),
  },
});
