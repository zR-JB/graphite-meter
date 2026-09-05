import { canFocus } from "./focus";

// Svelte action for flyout dialogs.
const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "textarea:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "details > summary:first-of-type",
  '[tabindex]:not([tabindex="-1"])',
].join(",");
export function focusTrap(node: HTMLElement, active = true) {
  let enabled = active;
  let focusTimer: number | undefined;
  // Closed disclosures retain layout boxes; check rendering visibility too.
  function getFocusable(): HTMLElement[] {
    return Array.from(
      node.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
    ).filter(canFocus);
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
  function cancelFocus() {
    window.clearTimeout(focusTimer);
    focusTimer = undefined;
  }
  function focusFirst() {
    // Resolve the target after opening, not before a pending render or teardown.
    focusTimer = window.setTimeout(() => {
      focusTimer = undefined;
      if (enabled && canFocus(node)) (getFocusable()[0] ?? node).focus();
    }, 0);
  }
  node.addEventListener("keydown", handleKeydown);
  if (enabled) focusFirst();
  return {
    update(nextActive: boolean) {
      if (enabled === nextActive) return;
      enabled = nextActive;
      cancelFocus();
      if (enabled) focusFirst();
    },
    destroy() {
      enabled = false;
      cancelFocus();
      node.removeEventListener("keydown", handleKeydown);
    },
  };
}
