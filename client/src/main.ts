import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";
import { STORAGE_KEY } from "./lib/state/persistence";

// The inline theme script in index.html gets the first frame right.
// This bundle loads after first paint, so it only covers that script going missing.
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
    // Corrupt or blocked storage: keep whatever the document already has.
  }
})();

mount(App, {
  target: document.getElementById("app")!,
});
