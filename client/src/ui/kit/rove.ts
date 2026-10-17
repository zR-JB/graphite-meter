// Roves the keyboard through a list of explained facts. The list is one tab stop, and arrows, Home and End move
// between its facts.
import { onCleanup } from "../../lib/signal";

const ROVE: Record<string, number> = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 };

/** Goes on a list of facts as its `ref`. Its facts are the explained words inside it. */
export function tipGroup(list: HTMLElement): void {
  // Explained words inside a group leave the tab order to it (tip.ts).
  list.dataset.tipGroup = "";
  const facts = () => [...list.querySelectorAll<HTMLElement>("[data-tip][tabindex]")];
  // The group keeps exactly one stop as facts come, go or gain or lose their tab index (tip.ts). Only a change writes.
  const settle = () => {
    const found = facts();
    const stop = found.find((fact) => fact.tabIndex === 0) ?? found[0];
    for (const fact of found) if ((fact === stop) !== (fact.tabIndex === 0)) fact.tabIndex = fact === stop ? 0 : -1;
  };
  const observer = new MutationObserver(settle);
  observer.observe(list, { childList: true, subtree: true, attributeFilter: ["tabindex"] });
  onCleanup(() => observer.disconnect());
  // The first settle waits until each fact has taken its place in the group.
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
