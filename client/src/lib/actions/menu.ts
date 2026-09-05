/** Flat action-menu navigation; components own visibility and action focus. */
export function focusMenuItem(menu: HTMLElement | undefined, last = false) {
  const items = menuItems(menu);
  focusItem(items, last ? items.length - 1 : 0);
}
function menuItems(menu: HTMLElement | undefined) {
  return Array.from(
    menu?.querySelectorAll<HTMLButtonElement>(
      '[role="menuitem"]:not(:disabled)',
    ) ?? [],
  );
}
function focusItem(items: HTMLButtonElement[], index: number) {
  for (const [position, item] of items.entries())
    item.tabIndex = position === index ? 0 : -1;
  items[index]?.focus({ preventScroll: true });
}
export function navigateMenu(
  event: KeyboardEvent,
  menu: HTMLElement | undefined,
  trigger: HTMLButtonElement | undefined,
  close: () => void,
) {
  if (event.key === "Escape" || event.key === "Tab") {
    event.stopPropagation();
    if (event.key === "Escape") event.preventDefault();
    // Native Tab starts at the invoker and leaves the menu in either direction.
    trigger?.focus({ preventScroll: true });
    close();
    return;
  }
  const items = menuItems(menu);
  if (!items.length) return;
  const current = items.findIndex((item) => item === document.activeElement);
  let next: number;
  switch (event.key) {
    case "ArrowDown":
      next = (current + 1) % items.length;
      break;
    case "ArrowUp":
      next = (current - 1 + items.length) % items.length;
      break;
    case "Home":
      next = 0;
      break;
    case "End":
      next = items.length - 1;
      break;
    default:
      return;
  }
  event.preventDefault();
  event.stopPropagation();
  focusItem(items, next);
}
