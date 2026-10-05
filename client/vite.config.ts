import { defineConfig, type Plugin } from "vite";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";
import { brotliCompressSync, gzipSync } from "node:zlib";
import { execFileSync } from "node:child_process";
import { svelte } from "@sveltejs/vite-plugin-svelte";

const env = process.env;

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
    if (!env.GM_LEGAL_SCAN_OUT) return;
    const output = resolve(env.GM_LEGAL_SCAN_OUT);
    if (!output.startsWith(resolve(tmpdir()) + sep))
      throw new Error(
        "GM_LEGAL_SCAN_OUT must name a file in the temporary directory",
      );
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

// Brotli and gzip copies of the text files let the server send them compressed at no per-request cost; the
// server injects into index.html, so it compresses nothing it rewrites.
const precompress = (): Plugin => {
  let outDir = "";
  return {
    name: "gm-precompress",
    apply: "build",
    configResolved: (config) => void (outDir = config.build.outDir),
    closeBundle: {
      order: "post",
      handler() {
        for (const name of readdirSync(outDir, { recursive: true })) {
          const path = join(outDir, String(name));
          if (!/\.(js|css|svg|json|txt)$/.test(path)) continue;
          const source = readFileSync(path);
          if (source.length < 1024) continue;
          writeFileSync(`${path}.br`, brotliCompressSync(source));
          writeFileSync(`${path}.gz`, gzipSync(source, { level: 9 }));
        }
      },
    },
  };
};

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
  plugins: [svelte(), versionFile(), minifyHtml(), legalScan(), precompress()],
  build: {
    outDir: env.GM_LEGAL_SCAN_DIR ?? "dist",
  },
  define: {
    __GM_BUILD_PROFILE__: JSON.stringify(buildProfile),
    __GM_RELEASE_VERSION__: JSON.stringify(releaseVersion),
    __GM_SOURCE_REVISION__: JSON.stringify(sourceRevision),
    __GM_CLIENT_VERSION__: JSON.stringify(clientVersion),
    __GM_BUILD_IDENTITY__: JSON.stringify(buildIdentity),
  },
});
