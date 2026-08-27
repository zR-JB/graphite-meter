// Reactive matchMedia wrapper for JS-visible breakpoints.
export function mediaQuery(query: string) {
  // Seeded eagerly. The effect starts at mount, and first render needs the real answer.
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
