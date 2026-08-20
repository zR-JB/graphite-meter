import getExePath from "../node_modules/@typescript/native/lib/getExePath.js";

const result = Bun.spawnSync(
  [getExePath(), "--noEmit", "-p", "./tsconfig.node.json"],
  {
    cwd: new URL("..", import.meta.url).pathname,
    stdout: "inherit",
    stderr: "inherit",
  },
);
process.exit(result.exitCode);
