// Without scroll-driven animations (Firefox), a scroller marks itself scrolled from its top, and the stylesheet
// draws the rule under its head that a scroll timeline draws elsewhere; it alone names the scrollers that matter.

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
