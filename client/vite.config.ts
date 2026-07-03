import { defineConfig, type Plugin } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import pkg from "./package.json";

// --- Build-time client configuration (see src/lib/buildenv.ts) -------------
// Driven by GM_CLIENT_* env vars (the justfile `prod` recipe / `docker build
// --build-arg` set them). Read from process.env here and injected via Vite
// `define` as RAW literal tokens — that literal substitution is what lets
// Rollup constant-fold + tree-shake the dummy runner and Developer tooling out
// of a production bundle. (`.env` / import.meta.env would yield the *string*
// "false", which is truthy and would defeat the tree-shaking.)
//
// Dev defaults (no env set): real engine, dummy + dev tools included, "dev"
// label — so `just dev` / `just build-client` behave as before, no changes.
const env = process.env;

const defaultEngine: "real" | "dummy" =
  env.GM_CLIENT_ENGINE === "dummy" ? "dummy" : "real";

// Boolean knobs default ON; only an explicit "0"/"false" turns them off.
const off = (v: string | undefined) => v === "0" || v === "false";
const allowDummy = !off(env.GM_CLIENT_ALLOW_DUMMY);
const devTools = !off(env.GM_CLIENT_DEV_TOOLS);

const buildLabel = env.GM_CLIENT_BUILD_LABEL ?? "dev";

// Canonical client version: package.json semver + the build label (git short
// hash in prod, "dev" otherwise) — e.g. "0.0.0+abc1234". Shown in the Endpoint
// info, sent to the server on preflight, and written to dist/version.json so
// whatever server hosts the static files can identify the bundle.
const clientVersion = `${pkg.version}+${buildLabel}`;

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

export default defineConfig({
  plugins: [svelte(), tailwindcss(), versionFile()],
  define: {
    __GM_DEFAULT_ENGINE__: JSON.stringify(defaultEngine), // "real" | "dummy"
    __GM_ALLOW_DUMMY__: JSON.stringify(allowDummy), // bare true | false
    __GM_DEV_TOOLS__: JSON.stringify(devTools), // bare true | false
    __GM_BUILD_LABEL__: JSON.stringify(buildLabel), // "abc1234"
    __GM_CLIENT_VERSION__: JSON.stringify(clientVersion), // "0.0.0+abc1234"
  },
});
