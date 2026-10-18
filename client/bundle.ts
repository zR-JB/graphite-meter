// The app's bundles, shared by the build and the screenshot pipeline: the workers first so the app can name them, then
// the app with the build's facts defined.
import type { BunPlugin } from "bun";
import { readdir } from "node:fs/promises";
import { basename, join, relative, sep } from "node:path";

export interface Facts {
  profile: "dev" | "prod";
  version: string | null;
  revision: string;
}

export async function bundles(outdir: string, { profile, version, revision }: Facts, plugins: BunPlugin[] = []) {
  const options = {
    outdir,
    minify: true,
    target: "browser",
    naming: "assets/[name]-[hash].[ext]",
    metafile: true,
  } as const;
  const asset = (path: string) => `/${relative(outdir, path).split(sep).join("/")}`;
  const workerDir = join(import.meta.dir, "src/engine/workers");
  const workerSources = (await readdir(workerDir))
    .filter((name) => name.endsWith(".ts") && !/\.test\.ts$|\.testutil\.ts$/.test(name))
    .map((name) => join(workerDir, name));
  const workers = await Bun.build({ ...options, entrypoints: workerSources });
  const define: Record<string, string> = {
    "BUILD.profile": JSON.stringify(profile),
    "BUILD.version": JSON.stringify(version),
    "BUILD.revision": JSON.stringify(revision),
    "BUILD.clientVersion": JSON.stringify(version ? `v${version}` : revision),
  };
  for (const output of workers.outputs.filter((each) => each.kind === "entry-point"))
    define[`WORKERS.${basename(output.path).replace(/-[^-]+\.js$/, "")}`] = JSON.stringify(asset(output.path));
  // Fonts keep their names under fonts/: the sign-in pages load two of them before the app exists.
  const app = await Bun.build({
    ...options,
    entrypoints: [join(import.meta.dir, "src/main.tsx")],
    define,
    external: ["/fonts/*"],
    plugins,
  });
  return { workers, app };
}
