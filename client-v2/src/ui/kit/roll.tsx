// A changed figure rolls like a counter: the old text leaves as the new one arrives, upward while its rank grows. When
// the words between the figures stay, as in 10 s to 11 s or 1 min 30 s to 1 min 45 s, only the figures that changed
// roll and the words hold still; a new shape, 59 s to 1 min, rolls whole.
import { reducedMotion } from "../../lib/motion";
import { effect, untrack } from "../../lib/signal";

const ROLL_MS = 320;
const ROLL_PX = 10;
const CUBIC_OUT = "cubic-bezier(0.33, 1, 0.68, 1)";
const CUBIC_IN = "cubic-bezier(0.32, 0, 0.67, 0)";

const FIGURE = /(\d+(?:\.\d+)?)/;
const parts = (text: string) => text.split(FIGURE).filter(Boolean);
const isFigure = (part: string) => /\d/.test(part);
/** The words between a text's figures: two texts of one shape roll figure by figure. */
const shape = (text: string) =>
  parts(text)
    .map((part) => (isFigure(part) ? "#" : part))
    .join("");

const span = (className: string, text: string) =>
  Object.assign(document.createElement("span"), { className, textContent: text });

/** Figures in ink, each in a slot of its own, the words between them quieter. */
function value(text: string): HTMLSpanElement {
  const shown = document.createElement("span");
  shown.className = "roll-value";
  for (const part of parts(text)) {
    if (!isFigure(part)) {
      shown.append(span("roll-unit", part));
      continue;
    }
    const slot = span("roll-slot", "");
    slot.append(span("roll-figure", part));
    shown.append(slot);
  }
  return shown;
}

/** Rolls `old` out and `next` in, both already in place; `old` is removed once it has left. */
function roll(old: HTMLElement, next: HTMLElement, direction: number): void {
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
  next.animate(
    [
      { translate: offset(1), opacity: 0 },
      { translate: "0 0", opacity: 1 },
    ],
    { duration: ROLL_MS, easing: CUBIC_OUT },
  );
}

export function Roll(props: { text: () => string; rank: () => number }): Node {
  const root = document.createElement("span");
  root.className = "roll";
  let shown: HTMLSpanElement | null = null;
  let shownText = "";
  let rank: number | null = null;
  let direction = 1;
  effect(() => {
    const text = props.text();
    const next = props.rank();
    untrack(() => {
      if (rank !== null && next !== rank) direction = next > rank ? 1 : -1;
      rank = next;
      if (shown && shownText === text) return;
      const still = !shown || reducedMotion.peek() || !root.checkVisibility();
      if (shown && !still && shape(text) === shape(shownText)) {
        // Same words: each changed figure rolls in its slot.
        const figures = parts(text).filter(isFigure);
        const slots = shown.querySelectorAll<HTMLElement>(".roll-slot");
        figures.forEach((figure, index) => {
          const slot = slots[index]!;
          const current = slot.lastElementChild as HTMLElement;
          if (current.textContent === figure) return;
          roll(current, slot.appendChild(span("roll-figure", figure)), direction);
        });
        shownText = text;
        return;
      }
      const old = shown;
      shown = root.appendChild(value(text));
      shownText = text;
      if (!old) return;
      if (still) return old.remove();
      roll(old, shown, direction);
    });
  });
  return root;
}
