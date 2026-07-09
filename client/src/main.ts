import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";
import { STORAGE_KEY } from "./lib/state/persistence";

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
  } catch {}
})();

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
