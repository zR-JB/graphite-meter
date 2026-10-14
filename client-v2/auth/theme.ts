// The saved theme on <html> before the first paint, for the app shell and the auth pages. With nothing stamped
// (storage blocked, scripting off) their stylesheets follow the OS preference.

/** The browser's own settings, shared by both clients' preferences, this script and the sign-in pages. */
export const STORAGE_KEY = "graphite-meter:v1";

export function prePaintTheme(): void {
  try {
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY) || "{}").theme;
    const system = matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
    document.documentElement.dataset.theme = saved === "light" || saved === "dark" ? saved : system;
  } catch {}
}
