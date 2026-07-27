import { defineConfig, type Plugin } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import pkg from "./package.json";

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
// The benchmark's tuning surface. Opt-in, unlike the knobs above: every build
// path that forgets it must produce a bundle without the tuning message.
const bench = env.GM_CLIENT_BENCH === "1";

const buildLabel = env.GM_CLIENT_BUILD_LABEL ?? "dev";

// Canonical client version: the VERSION build-arg/env (same value the server
// stamps into EngineVersion; see container/Dockerfile and justfile) plus the
// build label (git short hash in prod, "dev" otherwise), e.g. "0.1.0+abc1234".
// Falls back to package.json's "version" only when VERSION isn't set (plain
// `bun run build` outside just/Docker). That field is a frozen "0.0.0"
// sentinel, matching go/internal/config.EngineVersion's "0.0.0-dev" fallback.
// Never bump it by hand: every real version comes from the git tag via
// release.yml, so an untagged build has no version to reflect anyway.
const clientVersion = `${env.VERSION ?? pkg.version}+${buildLabel}`;

// Emit dist/version.json alongside the bundle (build only; dev serves no dist).
const versionFile = (): Plugin => ({
  name: "gm-version-file",
  generateBundle() {
    this.emitFile({
      type: "asset",
      fileName: "version.json",
      source: JSON.stringify({ version: clientVersion, label: buildLabel }),
    });
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
  plugins: [svelte(), tailwindcss(), versionFile(), minifyHtml()],
  define: {
    __GM_DEFAULT_ENGINE__: JSON.stringify(defaultEngine), // "real" | "dummy"
    __GM_ALLOW_DUMMY__: JSON.stringify(allowDummy), // bare true | false
    __GM_DEV_TOOLS__: JSON.stringify(devTools), // bare true | false
    __GM_BENCH__: JSON.stringify(bench), // bare true | false
    __GM_BUILD_LABEL__: JSON.stringify(buildLabel), // "abc1234"
    __GM_CLIENT_VERSION__: JSON.stringify(clientVersion), // "0.0.0+abc1234"
  },
});
