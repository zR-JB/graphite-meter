// Tips answer a hand that rests on them, not one passing by; a tip or readout warms its neighbours.
const WARM_MS = 600;
let warm = false;
let cooling = 0;
let open: (() => void) | null = null;

/** A tip or readout just closed: the next one nearby answers a short rest. */
export function warmUp() {
  warm = true;
  clearTimeout(cooling);
  // Not motion: the warm spell ends.
  cooling = window.setTimeout(() => (warm = false), WARM_MS);
}

/** While a tip is open or one just closed, the next answers a short rest, never a sweep. */
export const isWarm = () => warm || open !== null;

/** One tip at a time: opening one closes the last at once. */
export function claim(close: () => void) {
  if (open !== close) open?.();
  open = close;
}

export function release(close: () => void) {
  if (open === close) open = null;
}
