// Reactive matchMedia wrapper for JS-visible breakpoints. Create it from
// component init/reactive context because it owns Svelte runes.
export function mediaQuery(query: string) {
  // Seeded eagerly so the first render already has the real answer; the effect
  // below only starts after mount.
  let matches = $state(
    typeof window !== "undefined" ? window.matchMedia(query).matches : false,
  );

  $effect(() => {
    const media = window.matchMedia(query);
    const sync = () => (matches = media.matches);
    sync();
    media.addEventListener("change", sync);
    return () => media.removeEventListener("change", sync);
  });

  return {
    get matches() {
      return matches;
    },
  };
}
