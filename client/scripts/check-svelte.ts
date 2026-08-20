import { realpathSync } from "node:fs";

const root = new URL("..", import.meta.url).pathname;
const checker = `${root}/node_modules/svelte-check/bin/svelte-check`;
const candidate = Bun.which("node");
const node =
  candidate && realpathSync(candidate) !== realpathSync(process.execPath)
    ? candidate
    : null;
const command = node
  ? [
      node,
      checker,
      "--tsconfig",
      "./tsconfig.check.json",
      "--tsgo-experimental-api",
    ]
  : [process.execPath, checker, "--tsconfig", "./tsconfig.check.json"];

if (!node)
  console.warn(
    "check: Node is unavailable, so svelte-check is using its TS6 integration; CI exercises the TypeScript 7 experimental API",
  );
const result = Bun.spawnSync(command, {
  cwd: root,
  stdout: "inherit",
  stderr: "inherit",
});
process.exit(result.exitCode);
