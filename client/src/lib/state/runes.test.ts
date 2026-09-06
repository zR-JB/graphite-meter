import { plugin, Transpiler } from "bun";
import { compileModule } from "svelte/compiler";

const browserReactivity = import.meta
  .resolve("svelte/reactivity")
  .replace("index-server.js", "index-client.js");

plugin({
  name: "svelte-runes",
  setup(build) {
    build.onLoad({ filter: /\.svelte\.ts$/ }, async (args) => {
      const source = await Bun.file(args.path).text();
      const module = new Transpiler({ loader: "ts" }).transformSync(source);
      const compiled = compileModule(module, {
        generate: "client",
        filename: args.path,
      }).js.code;
      return {
        // Client-compiled stores need reactive browser collections rather than native server Maps.
        contents: compiled.replace(
          /from (["'])svelte\/reactivity\1/g,
          `from ${JSON.stringify(browserReactivity)}`,
        ),
        loader: "js",
      };
    });
  },
});
