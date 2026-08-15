import { defineConfig } from "vite";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// The transport E2E harness imports production lane/worker code, but it is not
// the product SPA. Build it once into static assets instead of keeping a Vite
// development server alive for the duration of the browser suite. This makes
// the E2E lifecycle independent of dev-server HMR/watch behavior.
const here = dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  root: here,
  build: {
    outDir: resolve(here, ".e2e-dist"),
    emptyOutDir: true,
    rollupOptions: {
      input: resolve(here, "bench/harness.html"),
    },
  },
});
