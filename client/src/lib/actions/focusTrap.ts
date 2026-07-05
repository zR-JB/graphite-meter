/* ============================================================
 * focusTrap — Svelte action
 * Tab-cycles focus within an open drawer and focuses the first
 * focusable element on open. Pure DOM, no SvelteKit.
 * ============================================================ */

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
    // Defer a frame so the node is laid out before focus moves into it.
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
