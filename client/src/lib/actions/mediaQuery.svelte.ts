/* ============================================================
 * mediaQuery — shared reactive matchMedia wrapper
 * A single, small primitive for every JS-visible breakpoint in
 * the app (side-panel dock/flyout, tooltip coarse-pointer
 * detection, …) instead of each consumer hand-rolling its own
 * matchMedia + addEventListener/onMount pair.
 *
 * Usage (inside a component's script, top level or in a
 * function called during component init — it uses $state/$effect
 * so it must run in a reactive context):
 *   const wide = mediaQuery("(min-width: 1200px)");
 *   ... wide.matches ...
 * ============================================================ */

export function mediaQuery(query: string) {
  let matches = $state(
    typeof window !== "undefined" ? window.matchMedia(query).matches : false,
  );

  $effect(() => {
    const mq = window.matchMedia(query);
    const apply = () => (matches = mq.matches);
    apply();
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
  });

  return {
    get matches() {
      return matches;
    },
  };
}
