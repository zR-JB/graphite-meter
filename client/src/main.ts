import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";
import { STORAGE_KEY } from "./lib/state/persistence";

/* Apply the saved theme as the bundle loads. The inline <head> script in
 * index.html runs first and prevents the white flash; this is a backstop
 * if the inline script was stripped/blocked. Falls back to system preference,
 * then to the :root dark default. */
(function applyThemePrePaint() {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    const saved = raw ? (JSON.parse(raw) as { theme?: string }).theme : null;
    const theme =
      saved === "light" || saved === "dark"
        ? saved
        : window.matchMedia?.("(prefers-color-scheme: light)").matches
          ? "light"
          : "dark";
    document.documentElement.setAttribute("data-theme", theme);
  } catch {
    /* corrupt/blocked storage → leave the :root dark default in place */
  }
})();

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
