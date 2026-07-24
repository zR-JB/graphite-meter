/* Pre-paint theme for the server-rendered auth pages: reads the same
   localStorage key as the app (STORAGE_KEY in src/lib/state/persistence.ts) and
   stamps data-theme on the root element ahead of first paint.
   go/internal/auth serves this file verbatim in every auth page head and pins
   its sha256 in the Content-Security-Policy, so it must stay dependency-free.
   With no attribute stamped (blocked storage, scripting off), auth.css follows
   the OS color preference. */
try {
  const raw = localStorage.getItem("graphite-meter:v1");
  const saved = raw ? JSON.parse(raw).theme : null;
  const theme =
    saved === "light" || saved === "dark"
      ? saved
      : matchMedia("(prefers-color-scheme: light)").matches
        ? "light"
        : "dark";
  document.documentElement.setAttribute("data-theme", theme);
} catch {}
