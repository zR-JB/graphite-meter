// Without scroll-driven animations (Firefox), a scroller marks itself scrolled so the stylesheet draws its rule.

export function markScrolled(): () => void {
  if (CSS.supports("animation-timeline: scroll()")) return () => {};
  const scrolled = (event: Event) => {
    const box = event.target;
    if (box instanceof HTMLElement && box.hasAttribute("data-scrolled") !== box.scrollTop > 0)
      box.toggleAttribute("data-scrolled");
  };
  document.addEventListener("scroll", scrolled, { capture: true, passive: true });
  return () => document.removeEventListener("scroll", scrolled, { capture: true });
}
