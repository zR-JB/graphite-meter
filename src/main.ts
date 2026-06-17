import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";
import { STORAGE_KEY } from "./lib/state/persistence";

/* Apply the saved theme BEFORE first paint (§14.1) so there is no
 * dark→light flash on reload. A tiny synchronous localStorage read —
 * the store re-applies the same value via its $effect once it mounts,
 * but doing it here first means the very first frame is already correct.
 * Falls back to the system preference, then to the :root dark default. */
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
