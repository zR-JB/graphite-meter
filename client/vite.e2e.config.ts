import { defineConfig } from "vite";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Build the transport harness as static assets so E2E does not depend on a Vite dev server or HMR.
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
