/** Focus may only return to a connected, visible, interactive surface. */
export function canFocus(
  target: HTMLElement | null | undefined,
): target is HTMLElement {
  return !!(
    target?.isConnected &&
    !target.closest('[inert], [aria-hidden="true"]') &&
    !target.matches(":disabled") &&
    target.checkVisibility({ visibilityProperty: true })
  );
}

/** A late completion must not replace focus the user has already moved. */
export function hasFocus(): boolean {
  const active = document.activeElement;
  return (
    active instanceof HTMLElement &&
    active !== document.body &&
    active !== document.documentElement &&
    canFocus(active)
  );
}

export function activeModal(): HTMLElement | null {
  const modals = document.querySelectorAll<HTMLElement>("dialog:modal");
  return modals[modals.length - 1] ?? null;
}
