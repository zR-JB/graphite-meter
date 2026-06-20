import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

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

export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  define: {
    __GM_DEFAULT_ENGINE__: JSON.stringify(defaultEngine), // "real" | "dummy"
    __GM_ALLOW_DUMMY__: JSON.stringify(allowDummy), // bare true | false
    __GM_DEV_TOOLS__: JSON.stringify(devTools), // bare true | false
    __GM_BUILD_LABEL__: JSON.stringify(buildLabel), // "abc1234"
  },
});
