const root = new URL("..", import.meta.url).pathname;
const checker = `${root}/node_modules/svelte-check/bin/svelte-check`;
const command = [
  process.execPath,
  checker,
  "--tsconfig",
  "./tsconfig.check.json",
  "--tsgo",
];
const result = Bun.spawnSync(command, {
  cwd: root,
  stdout: "inherit",
  stderr: "inherit",
});
process.exit(result.exitCode);
