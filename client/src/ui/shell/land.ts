// Content that stops rendering never advances its animations, so what is still under way there lands at its end.

export function land(root: Element): void {
  for (const animation of root.getAnimations({ subtree: true })) {
    const end = animation.effect?.getComputedTiming().endTime;
    if (animation.timeline === document.timeline && typeof end === "number" && Number.isFinite(end))
      animation.finish();
  }
}
