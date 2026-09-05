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

/** An existing modal retains focus while its underlying workspace changes. */
export function activeModal(): HTMLElement | null {
  const active = document.activeElement;
  if (!(active instanceof HTMLElement)) return null;
  const modal = active.closest<HTMLElement>('[aria-modal="true"]');
  return canFocus(modal) ? modal : null;
}
