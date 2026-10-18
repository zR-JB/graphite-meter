// Where focus may go: a late completion never moves focus the user already moved, and never into hidden surfaces.

/** Holds when the target is connected, visible, interactive, and outside inert or hidden surfaces. */
export function canFocus(target: Element | null | undefined): target is HTMLElement {
  return (
    target instanceof HTMLElement &&
    target.isConnected &&
    !target.closest('[inert], [aria-hidden="true"]') &&
    !target.matches(":disabled") &&
    target.checkVisibility({ visibilityProperty: true })
  );
}

export function hasFocus(): boolean {
  const active = document.activeElement;
  return active !== document.body && active !== document.documentElement && canFocus(active);
}

export const activeModal = (): HTMLElement | null =>
  [...document.querySelectorAll<HTMLElement>("dialog:modal")].at(-1) ?? null;

/** Text entry keeps its own keys. */
export const isEditable = (target: EventTarget | null) =>
  target instanceof HTMLElement &&
  (target.isContentEditable ||
    target.matches("textarea, select, input:not([type='checkbox'], [type='radio'])"));
