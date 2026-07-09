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
