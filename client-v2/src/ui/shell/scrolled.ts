// Without scroll-driven animations (Firefox), a scroller marks itself scrolled from its top, and the stylesheet
// draws the rule under its head that a scroll timeline draws elsewhere. Only a change writes.

const SCROLLERS = ".sheet-body, .detail-pane, .history-list";

export function markScrolled(): () => void {
  if (CSS.supports("animation-timeline: scroll()")) return () => {};
  const scrolled = (event: Event) => {
    const box = event.target;
    if (box instanceof HTMLElement && box.matches(SCROLLERS)) box.toggleAttribute("data-scrolled", box.scrollTop > 0);
  };
  document.addEventListener("scroll", scrolled, { capture: true, passive: true });
  return () => document.removeEventListener("scroll", scrolled, { capture: true });
}
