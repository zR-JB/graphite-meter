// Temporary gallery step: renders hero.html, which frames two captures, into out/hero.png.
import { open, quit, sleep } from "./cdp";

const server = Bun.serve({
  port: 8771,
  hostname: "127.0.0.1",
  fetch: (req) => {
    const name = new URL(req.url).pathname.slice(1);
    if (name === "client/public/fonts/ibm-plex-sans-var-latin1.woff2")
      return new Response(Bun.file(`${import.meta.dir}/../../../client/public/fonts/ibm-plex-sans-var-latin1.woff2`));
    return name === "hero.html" || name === "out/hero-desktop.png" || name === "out/mobile.png"
      ? new Response(Bun.file(`${import.meta.dir}/${name}`))
      : new Response("not found", { status: 404 });
  },
});
const s = await open("http://127.0.0.1:8771/hero.html", 2400, 1400);
for (let i = 0; i < 50 && !(await s.evaluate(`document.body.dataset.ready === "1"`)); i++) await sleep(100);
await s.evaluate(`document.fonts.ready.then(() => true)`);
await sleep(300);
await s.shot("hero");
console.log("hero", s.errors);
await quit();
server.stop();
process.exit(0);
