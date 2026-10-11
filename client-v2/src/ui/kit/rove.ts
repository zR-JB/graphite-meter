// Keyboard roving through a list of explained facts: the list is one tab stop, and arrows, Home and End move between
// its facts.
import { onCleanup } from "../../lib/signal";

const ROVE: Record<string, number> = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 };

/** Use as `ref` on a `[data-tip-group]` list; its facts are the explained words inside it. */
export function tipGroup(list: HTMLElement): void {
  const facts = () => [...list.querySelectorAll<HTMLElement>("[data-tip][tabindex]")];
  const settle = () => {
    const found = facts();
    if (found[0] && !found.some((fact) => fact.tabIndex === 0)) found[0].tabIndex = 0;
  };
  const observer = new MutationObserver(settle);
  observer.observe(list, { childList: true, subtree: true });
  onCleanup(() => observer.disconnect());
  // After each fact has taken its place in the group.
  queueMicrotask(settle);
  list.addEventListener("keydown", (event) => {
    const found = facts();
    const at = found.indexOf(event.target as HTMLElement);
    const step = ROVE[event.key];
    const to =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? found.length - 1
          : step
            ? (at + step + found.length) % found.length
            : -1;
    if (at < 0 || to < 0) return;
    event.preventDefault();
    found[at]!.tabIndex = -1;
    found[to]!.tabIndex = 0;
    found[to]!.focus();
  });
}
