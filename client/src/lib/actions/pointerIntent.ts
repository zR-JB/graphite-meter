/* ============================================================
 * pointerIntent — Svelte action
 * Sets `--intent-x` / `--intent-y` CSS custom properties to the
 * pointer's position within the node on pointermove, enabling
 * radial hover effects. Pure DOM, no SvelteKit.
 * ============================================================ */

export interface PointerIntentOptions {
  disabled?: boolean;
}

export function pointerIntent(
  node: HTMLElement,
  options: PointerIntentOptions = {},
) {
  let opts = options;

  function setPointer(event: PointerEvent) {
    if (opts.disabled) return;
    const rect = node.getBoundingClientRect();
    node.style.setProperty("--intent-x", `${event.clientX - rect.left}px`);
    node.style.setProperty("--intent-y", `${event.clientY - rect.top}px`);
  }

  function clearPointer() {
    node.style.removeProperty("--intent-x");
    node.style.removeProperty("--intent-y");
  }

  node.addEventListener("pointermove", setPointer);
  node.addEventListener("pointerleave", clearPointer);

  return {
    update(nextOptions: PointerIntentOptions = {}) {
      opts = nextOptions;
      if (opts.disabled) clearPointer();
    },
    destroy() {
      node.removeEventListener("pointermove", setPointer);
      node.removeEventListener("pointerleave", clearPointer);
    },
  };
}
