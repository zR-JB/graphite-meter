import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";
import { STORAGE_KEY } from "./lib/state/persistence";

// Backstop for the inline theme script in index.html, which is what actually
// gets the first frame right; this only covers the case where that script was
// blocked or stripped, since the bundle loads well after first paint.
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
