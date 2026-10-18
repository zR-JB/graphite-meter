// How the console lays out at this viewport: whether panels dock beside the stage (two, one) or fly over it, and the
// dock widths. It stamps the mode on <html> for the stylesheet.
import { nextFrame } from "../../lib/frame";
import { computed, effect, onCleanup, signal, type Read } from "../../lib/signal";

export const DOCK = { min: 360, max: 720, preferred: 420 } as const;
export const clampDock = (px: number) => Math.min(DOCK.max, Math.max(DOCK.min, px));
/** The stage keeps this much beside docked panels, its padding included. */
const STAGE_MIN = 800;
const ONE_DOCK = 1_200;
const TWO_DOCKS = 2 * DOCK.min + STAGE_MIN;

export type Mode = "docked2" | "docked1" | "flyout";
export type Side = "left" | "right";
export interface DockWidths {
  /** The grid column, 0 when closed. */
  column: number;
  /** The most the handle may give this side now. */
  max: number;
}

/** Each open side within 360 to 720 px, the stage keeping 800 px. Past the budget the excess comes out of each side in
    proportion to how far it sits above the minimum, except that `priority` (the side resized last) keeps its own
    width so a dragged edge follows the pointer. Preferences never change here. */
export function resolveDocks(
  width: number,
  preferred: Record<Side, number>,
  open: Record<Side, boolean>,
  priority: Side | null = null,
): Record<Side, DockWidths> {
  const want = { left: open.left ? clampDock(preferred.left) : 0, right: open.right ? clampDock(preferred.right) : 0 };
  const count = +open.left + +open.right;
  const budget = Math.max(count * DOCK.min, width - STAGE_MIN);
  let { left, right } = want;
  if (left + right > budget) {
    if (priority && open.left && open.right) {
      // The other side takes the rest, which never exceeds what it wants.
      const kept = Math.min(want[priority], budget - DOCK.min);
      [left, right] = priority === "left" ? [kept, budget - kept] : [budget - kept, kept];
    } else {
      const extra = budget - count * DOCK.min;
      const above = left + right - count * DOCK.min;
      left = left ? DOCK.min + Math.floor((extra * (left - DOCK.min)) / above) : 0;
      right = right ? budget - left : 0;
    }
  }
  const stage = Math.min(STAGE_MIN, width - DOCK.min * count);
  return {
    left: { column: left, max: Math.min(DOCK.max, width - stage - right) },
    right: { column: right, max: Math.min(DOCK.max, width - stage - left) },
  };
}

export interface Layout {
  mode: Read<Mode>;
  /** Flyouts rise from the bottom: a phone held upright. */
  bottomSheet: Read<boolean>;
  /** The console's width in px. */
  width: Read<number>;
}

export function media(query: string): Read<boolean> {
  const list = matchMedia(query);
  const matches = signal(list.matches);
  const change = () => matches.set(list.matches);
  list.addEventListener("change", change);
  onCleanup(() => list.removeEventListener("change", change));
  return matches;
}

/** Follows the viewport and the console's own width (read once per frame at most, never inside observer delivery).
    Its listeners and observer live as long as the owner that creates it. */
export function createLayout(console: HTMLElement, root = document.documentElement): Layout {
  const two = media(`(min-width: ${TWO_DOCKS}px)`);
  const one = media(`(min-width: ${ONE_DOCK}px)`);
  const bottomSheet = media("(max-width: 759px) and (orientation: portrait)");
  const mode = computed<Mode>(() => (two() ? "docked2" : one() ? "docked1" : "flyout"));
  const width = signal(console.clientWidth);
  let pending: (() => void) | null = null;
  const observer = new ResizeObserver(() => {
    pending ??= nextFrame(() => {
      pending = null;
      width.set(console.clientWidth);
    });
  });
  observer.observe(console);
  onCleanup(() => {
    observer.disconnect();
    pending?.();
  });
  effect(() => {
    root.dataset.layout = mode();
    root.toggleAttribute("data-bottom-sheet", bottomSheet());
  });
  return { mode, bottomSheet, width };
}
