// The marks preferences leave on the page: the resolved theme, the browser bar's color, and how explanations open.
import type { PreferenceSignals } from "../../app/preferences";
import { computed, effect, signal, type Read } from "../../lib/signal";

/** Keeps `<html data-theme>`, the theme-color metas and `data-explanations` in step with the preferences; `auto`
    follows the system live. Returns the resolved theme. */
export function markDocument(preferences: Pick<PreferenceSignals, "theme" | "explanations">): Read<"light" | "dark"> {
  const query = matchMedia("(prefers-color-scheme: light)");
  const systemLight = signal(query.matches);
  query.addEventListener("change", () => systemLight.set(query.matches));
  const resolved = computed(() => {
    const theme = preferences.theme();
    return theme === "auto" ? (systemLight() ? "light" : "dark") : theme;
  });
  const root = document.documentElement;
  effect(() => {
    const theme = resolved();
    root.dataset.theme = theme;
    // The browser's own bar takes the chosen theme's canvas, not the system's.
    for (const meta of document.querySelectorAll<HTMLMetaElement>("meta[data-scheme]"))
      meta.media = meta.dataset.scheme === theme ? "all" : "not all";
  });
  effect(() => {
    root.dataset.explanations = preferences.explanations();
  });
  return resolved;
}
