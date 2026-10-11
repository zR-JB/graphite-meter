// A changed figure rolls like a counter: the old text leaves as the new one arrives, upward while its rank grows.
import { reducedMotion } from "../../lib/motion";
import { effect, untrack } from "../../lib/signal";

const ROLL_MS = 320;
const ROLL_PX = 10;
const CUBIC_OUT = "cubic-bezier(0.33, 1, 0.68, 1)";
const CUBIC_IN = "cubic-bezier(0.32, 0, 0.67, 0)";

/** Figures in ink, the words between them quieter. */
function value(text: string): HTMLSpanElement {
  const span = document.createElement("span");
  span.className = "roll-value";
  for (const part of text.split(/(\d+(?:\.\d+)?)/)) {
    if (!part) continue;
    if (/\d/.test(part)) span.append(part);
    else span.append(Object.assign(document.createElement("span"), { className: "roll-unit", textContent: part }));
  }
  return span;
}

export function Roll(props: { text: () => string; rank: () => number }): Node {
  const roll = document.createElement("span");
  roll.className = "roll";
  let shown: HTMLSpanElement | null = null;
  let rank: number | null = null;
  let direction = 1;
  effect(() => {
    const text = props.text();
    const next = props.rank();
    untrack(() => {
      if (rank !== null && next !== rank) direction = next > rank ? 1 : -1;
      rank = next;
      if (shown?.textContent === text) return;
      const old = shown;
      shown = roll.appendChild(value(text));
      if (!old) return;
      if (reducedMotion.peek() || !roll.checkVisibility()) return old.remove();
      const offset = (side: number) => `0 ${side * direction * ROLL_PX}px`;
      const leaving = old.animate(
        [
          { translate: "0 0", opacity: 1 },
          { translate: offset(-1), opacity: 0 },
        ],
        { duration: ROLL_MS, easing: CUBIC_IN, fill: "forwards" },
      );
      // A finished animation that still fills would hold the old text after it leaves.
      leaving.onfinish = () => {
        old.remove();
        leaving.cancel();
        leaving.effect = null;
      };
      shown.animate(
        [
          { translate: offset(1), opacity: 0 },
          { translate: "0 0", opacity: 1 },
        ],
        { duration: ROLL_MS, easing: CUBIC_OUT },
      );
    });
  });
  return roll;
}
