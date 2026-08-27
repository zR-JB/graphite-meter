// Svelte action for flyout dialogs.
const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "textarea:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");
export function focusTrap(node: HTMLElement, active = true) {
  let enabled = active;
  // A null offsetParent means a hidden subtree. Tab cannot reach it, so it stays out of the cycle.
  function getFocusable(): HTMLElement[] {
    return Array.from(
      node.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
    ).filter((el) => !el.hasAttribute("disabled") && el.offsetParent !== null);
  }
  function handleKeydown(event: KeyboardEvent) {
    if (!enabled || event.key !== "Tab") return;
    const focusable = getFocusable();
    if (!focusable.length) {
      event.preventDefault();
      node.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
      return;
    }
    if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }
  function focusFirst() {
    const first = getFocusable()[0];
    // Deferred one task: focusing an element still hidden mid-open is a no-op.
    window.setTimeout(() => (first ?? node).focus(), 0);
  }
  node.addEventListener("keydown", handleKeydown);
  if (enabled) focusFirst();
  return {
    update(nextActive: boolean) {
      enabled = nextActive;
      if (enabled) focusFirst();
    },
    destroy() {
      node.removeEventListener("keydown", handleKeydown);
    },
  };
}
