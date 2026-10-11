// Builds the browser app into dist/ for the servers to embed: content-hashed bundles under assets/, the shell, the
// shared public files, version.json and precompressed copies. Workers are bundled first so the app can name them.
import { cp, readdir, rm } from "node:fs/promises";
import { basename, join, relative, resolve, sep } from "node:path";
import { tmpdir } from "node:os";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";

const env = process.env;
const root = import.meta.dir;
const outdir = env.GM_LEGAL_SCAN_DIR ? resolve(env.GM_LEGAL_SCAN_DIR) : join(root, "dist");
// The build empties its output first: only dist/ or a directory inside the temporary one may be named.
if (outdir !== join(root, "dist") && !outdir.startsWith(resolve(tmpdir()) + sep))
  throw new Error("GM_LEGAL_SCAN_DIR must name a directory inside the temporary directory");
const profile = env.GM_CLIENT_BUILD_PROFILE === "prod" ? "prod" : "dev";
const version = env.VERSION || null;
const gitRevision = () => {
  try {
    return Bun.spawnSync(["git", "rev-parse", "--short", "HEAD"], { cwd: root }).stdout.toString().trim();
  } catch {
    return "";
  }
};
const revision = env.GM_CLIENT_REVISION || gitRevision() || "source";
const clientVersion = version ? `v${version}` : revision;

const naming = "assets/[name]-[hash].[ext]";
const options = { outdir, minify: true, target: "browser", naming, metafile: true } as const;
const asset = (path: string) => `/${relative(outdir, path).split(sep).join("/")}`;

await rm(outdir, { recursive: true, force: true });

const workerSources = (await readdir(join(root, "src/engine/workers")))
  .filter((name) => name.endsWith(".ts") && !/\.test\.ts$|\.testutil\.ts$/.test(name))
  .map((name) => join(root, "src/engine/workers", name));
const workers = await Bun.build({ ...options, entrypoints: workerSources });
const define: Record<string, string> = {
  "BUILD.profile": JSON.stringify(profile),
  "BUILD.version": JSON.stringify(version),
  "BUILD.revision": JSON.stringify(revision),
  "BUILD.clientVersion": JSON.stringify(clientVersion),
};
for (const output of workers.outputs.filter((each) => each.kind === "entry-point"))
  define[`WORKERS.${basename(output.path).replace(/-[^-]+\.js$/, "")}`] = JSON.stringify(asset(output.path));

// Fonts keep their names under fonts/: the sign-in pages load two of them before the app exists.
const app = await Bun.build({
  ...options,
  entrypoints: [join(root, "src/main.tsx")],
  define,
  splitting: true,
  external: ["/fonts/*"],
});
const entry = app.outputs.find((output) => output.kind === "entry-point")!;
const styles = app.outputs.filter((output) => output.path.endsWith(".css"));

// The servers hash the shell's one inline script and style into their content security policy and insert their meta
// tags before its one </head>.
const inline = async (code: string, loader: "js" | "css") => {
  const path = `/inline.${loader}`;
  const result = await Bun.build({ entrypoints: [path], files: { [path]: code }, minify: true });
  return (await result.outputs[0]!.text()).trim();
};
let shell = await Bun.file(join(root, "index.html")).text();
for (const tag of ["style", "script"] as const) {
  const [block, code] = new RegExp(`<${tag}>([\\s\\S]*?)</${tag}>`).exec(shell) ?? [];
  const minified = block && `<${tag}>${await inline(code!, tag === "style" ? "css" : "js")}</${tag}>`;
  if (minified) shell = shell.replace(block, () => minified);
}
const bundle = [
  ...styles.map((output) => `<link rel="stylesheet" href="${asset(output.path)}">`),
  `<script type="module" src="${asset(entry.path)}"></script>`,
].join("");
// Line breaks between tags go; inside text they stay one space.
shell = shell
  .replace("<!-- bundle -->", () => bundle)
  .replace(/<!--[\s\S]*?-->/g, "")
  .replace(/>\s*\n\s*</g, "><")
  .replace(/\s*\n\s*/g, " ")
  .trim();
await Bun.write(join(outdir, "index.html"), shell);

// Fonts, the favicon and the legal notices keep one owner while both clients ship.
await cp(join(root, "../client/public"), outdir, { recursive: true });
await Bun.write(join(outdir, "version.json"), JSON.stringify({ version, label: profile, revision }));

const files = (await readdir(outdir, { recursive: true, withFileTypes: true })).filter((each) => each.isFile());
for (const file of files) {
  const path = join(file.parentPath, file.name);
  if (!/\.(js|css|svg|json|txt)$/.test(file.name)) continue;
  const source = new Uint8Array(await Bun.file(path).arrayBuffer());
  if (source.length < 1024) continue;
  await Bun.write(`${path}.br`, brotliCompressSync(source, { params: { [constants.BROTLI_PARAM_QUALITY]: 11 } }));
  await Bun.write(`${path}.gz`, gzipSync(source, { level: 9 }));
}

validate(shell, (await readdir(outdir, { recursive: true })).map(String));

if (env.GM_LEGAL_SCAN_OUT) {
  const scan = resolve(env.GM_LEGAL_SCAN_OUT);
  if (!scan.startsWith(resolve(tmpdir()) + sep))
    throw new Error("GM_LEGAL_SCAN_OUT must name a file in the temporary directory");
  const modules = new Set<string>();
  for (const build of [workers, app])
    for (const input of Object.keys(build.metafile!.inputs)) modules.add(resolve(input));
  await Bun.write(scan, `${JSON.stringify([...modules].sort())}\n`);
}

console.log(`built ${profile} ${clientVersion} into ${relative(process.cwd(), outdir) || "."}`);

/** The contract both servers rely on (Go's embed, Rust's asset scan): checked here, since nothing else checks v2. */
function validate(html: string, names: string[]): void {
  const count = (needle: string) => html.split(needle).length - 1;
  const problems = [
    count("</head>") !== 1 && "the shell needs exactly one </head>",
    count("<script>") !== 1 && "the shell needs exactly one inline <script>",
    count("<style>") !== 1 && "the shell needs exactly one inline <style>",
    !html.includes('<div id="app"') && 'the shell needs <div id="app">',
    /graphite-meter-(auth|result-history-default)/.test(html) && "the shell must not carry the servers' meta tags",
    ...names.map((name) => {
      const parts = name.split(/[\\/]/);
      if (parts.some((part) => !/^[A-Za-z0-9_-][A-Za-z0-9._-]*$/.test(part) || part.includes("..")))
        return `unsafe asset name ${name}`;
      if (
        /\.(html|js|mjs|css|json|map|svg|png|jpe?g|webp|gif|ico|woff2?|ttf|wasm|txt)(\.br|\.gz)?$/.test(name) ||
        !name.includes(".")
      )
        return null;
      return `unsupported asset type ${name}`;
    }),
  ].filter(Boolean);
  if (problems.length) throw new Error(`invalid build:\n${problems.join("\n")}`);
}
